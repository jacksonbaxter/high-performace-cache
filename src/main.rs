use actix_web::{web, App, HttpServer, HttpResponse, Error as ActixError};
use serde::{Deserialize};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;
use std::collections::HashMap;
use thiserror::Error;
use redis::AsyncCommands;
use sqlx::postgres::PgPoolOptions;
use bb8_redis::{bb8, RedisConnectionManager};

#[derive(Clone)]
pub struct RedisPool {
    pool: Arc<bb8::Pool<RedisConnectionManager>>,
}

impl RedisPool {
    pub async fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let manager = RedisConnectionManager::new(redis_url)?;
        let pool = bb8::Pool::builder()
            .max_size(100)  // Increased from 16
            .min_idle(Some(20))  // Increased from 4
            .build(manager)
            .await
            .map_err(|e| redis::RedisError::from((redis::ErrorKind::IoError, "Pool creation failed", e.to_string())))?;

        Ok(RedisPool {
            pool: Arc::new(pool),
        })
    }

    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, redis::RedisError> {
        let mut conn = self.pool.get().await.map_err(|e| {
            redis::RedisError::from((redis::ErrorKind::IoError, "Failed to get connection", e.to_string()))
        })?;

        let result: Option<Vec<u8>> = conn.get(key).await?;
        Ok(result)
    }

    pub async fn set_ex(&self, key: &str, value: &[u8], ttl: u64) -> Result<(), redis::RedisError> {
        let mut conn = self.pool.get().await.map_err(|e| {
            redis::RedisError::from((redis::ErrorKind::IoError, "Failed to get connection", e.to_string()))
        })?;

        let _: () = redis::cmd("SETEX")
            .arg(key)
            .arg(ttl)
            .arg(value)
            .query_async(&mut *conn)
            .await?;

        Ok(())
    }

    pub async fn del(&self, key: &str) -> Result<(), redis::RedisError> {
        let mut conn = self.pool.get().await.map_err(|e| {
            redis::RedisError::from((redis::ErrorKind::IoError, "Failed to get connection", e.to_string()))
        })?;

        let _: () = conn.del(key).await?;
        Ok(())
    }
}

// Error types
#[derive(Error, Debug)]
pub enum CacheError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),
    
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    
    #[error("Item not found: {0}")]
    NotFound(String),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

// Configuration
#[derive(Debug, Deserialize, Clone)]
struct Config {
    redis_url: String,
    database_url: String,
    memory_cache_ttl: u64,
    redis_cache_ttl: u64,
    max_memory_items: usize,
}

// Cache entry with timestamp
#[derive(Clone, Debug)]
struct CacheEntry {
    data: Vec<u8>,
    timestamp: SystemTime,
}

// Memory cache implementation
struct MemoryCache {
    data: HashMap<String, CacheEntry>,
    ttl: Duration,
    max_items: usize,
}

impl MemoryCache {
    fn new(ttl: Duration, max_items: usize) -> Self {
        MemoryCache {
            data: HashMap::new(),
            ttl,
            max_items,
        }
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.data.get(key).and_then(|entry| {
            if entry.timestamp.elapsed().unwrap() < self.ttl {
                Some(entry.data.clone())
            } else {
                None
            }
        })
    }

    fn set(&mut self, key: String, value: Vec<u8>) {
        if self.data.len() >= self.max_items {
            // Remove oldest entry
            if let Some((oldest_key, _)) = self.data
                .iter()
                .min_by_key(|(_, entry)| entry.timestamp)
            {
                self.data.remove(&oldest_key.to_string());
            }
        }

        self.data.insert(key, CacheEntry {
            data: value,
            timestamp: SystemTime::now(),
        });
    }
}

// Main cache service
struct CacheService {
    memory_cache: Arc<RwLock<MemoryCache>>,
    redis_pool: RedisPool,
    db: PgPool,
    config: Config,
}

impl Clone for CacheService {
    fn clone(&self) -> Self {
        CacheService {
            memory_cache: self.memory_cache.clone(),
            redis_pool: self.redis_pool.clone(),
            db: self.db.clone(),
            config: self.config.clone(),
        }
    }
}

impl CacheService {
    async fn new(config: Config) -> Result<Self, CacheError> {
        let memory_cache = Arc::new(RwLock::new(MemoryCache::new(
            Duration::from_secs(config.memory_cache_ttl),
            config.max_memory_items,
        )));

        // Use the new Redis pool
        let redis_pool = RedisPool::new(&config.redis_url).await?;
        
        // Configure PostgreSQL pool with more conservative settings
        let db = PgPoolOptions::new()
            .max_connections(100)          // Increased from 32
            .min_connections(20)           // Increased from 4
            .acquire_timeout(Duration::from_secs(3))
            .connect(&config.database_url)
            .await
            .map_err(CacheError::Database)?;

        Ok(CacheService {
            memory_cache,
            redis_pool,
            db,
            config,
        })
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError> {
        // Try memory cache first with a read lock
        if let Some(data) = self.memory_cache.read().await.get(key) {
            return Ok(data);
        }
        
        // Try Redis and DB lookups concurrently
        let (redis_result, db_result) = tokio::join!(
            self.redis_pool.get(key),
            sqlx::query_as::<_, (Vec<u8>,)>("SELECT data FROM cached_data WHERE key = $1")
                .bind(key)
                .fetch_optional(&self.db)
        );

        // Check Redis result first
        if let Ok(Some(data)) = redis_result {
            // Update memory cache in the background
            let data_clone = data.clone();
            let key = key.to_string();
            let memory_cache = self.memory_cache.clone();
            tokio::spawn(async move {
                memory_cache.write().await.set(key, data_clone);
            });
            return Ok(data);
        }

        // Check DB result
        match db_result? {
            Some((data,)) => {
                // Update caches in the background
                let data_clone = data.clone();
                let key = key.to_string();
                let redis_pool = self.redis_pool.clone();
                let memory_cache = self.memory_cache.clone();
                let ttl = self.config.redis_cache_ttl;
                tokio::spawn(async move {
                    let _ = redis_pool.set_ex(&key, &data_clone, ttl).await;
                    memory_cache.write().await.set(key, data_clone);
                });
                Ok(data)
            }
            None => Err(CacheError::NotFound(key.to_string())),
        }
    }

    async fn set(&self, key: String, value: Vec<u8>) -> Result<(), CacheError> {
        // Update all caches concurrently
        let (db_result, redis_result) = tokio::join!(
            sqlx::query(
                "INSERT INTO cached_data (key, data) VALUES ($1, $2) 
                 ON CONFLICT (key) DO UPDATE SET data = EXCLUDED.data"
            )
            .bind(&key)
            .bind(&value)
            .execute(&self.db),
            self.redis_pool.set_ex(
                &key,
                &value,
                self.config.redis_cache_ttl
            )
        );

        // Update memory cache immediately (it's the fastest)
        self.memory_cache.write().await.set(key, value);

        // Check results
        db_result?;
        redis_result?;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        // Remove from database
        sqlx::query("DELETE FROM cached_data WHERE key = $1")
            .bind(key)
            .execute(&self.db)
            .await?;

        // Remove from Redis
        self.redis_pool.del(key).await?;

        // Remove from memory cache
        self.memory_cache.write().await.data.remove(key);

        Ok(())
    }
}

// API handlers
async fn get_cached_data(
    key: web::Path<String>,
    service: web::Data<CacheService>,
) -> Result<HttpResponse, ActixError> {
    match service.get(&key).await {
        Ok(data) => Ok(HttpResponse::Ok().body(data)),
        Err(CacheError::NotFound(_)) => Ok(HttpResponse::NotFound().finish()),
        Err(e) => {
            tracing::error!("Error getting cached data: {:?}", e);
            Ok(HttpResponse::InternalServerError().finish())
        }
    }
}

async fn set_cached_data(
    key: web::Path<String>,
    body: web::Bytes,
    service: web::Data<CacheService>,
) -> Result<HttpResponse, ActixError> {
    match service.set(key.into_inner(), body.to_vec()).await {
        Ok(_) => Ok(HttpResponse::Ok().finish()),
        Err(e) => {
            tracing::error!("Error setting cached data: {:?}", e);
            Ok(HttpResponse::InternalServerError().finish())
        }
    }
}

async fn delete_cached_data(
    key: web::Path<String>,
    service: web::Data<CacheService>,
) -> Result<HttpResponse, ActixError> {
    match service.delete(&key).await {
        Ok(_) => Ok(HttpResponse::Ok().finish()),
        Err(e) => {
            tracing::error!("Error deleting cached data: {:?}", e);
            Ok(HttpResponse::InternalServerError().finish())
        }
    }
}

async fn health_check() -> HttpResponse {
    HttpResponse::Ok().finish()
}

async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    // Run migrations in a transaction
    let mut transaction = pool.begin().await?;

    // Create the table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS cached_data (
            key VARCHAR(255) PRIMARY KEY,
            data BYTEA NOT NULL,
            created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&mut *transaction)
    .await?;

    // Create the trigger function
    sqlx::query(
        r#"
        CREATE OR REPLACE FUNCTION update_updated_at_column()
        RETURNS TRIGGER AS $$
        BEGIN
            NEW.updated_at = CURRENT_TIMESTAMP;
            RETURN NEW;
        END;
        $$ language 'plpgsql'
        "#,
    )
    .execute(&mut *transaction)
    .await?;

    // Drop the existing trigger if it exists
    sqlx::query(
        r#"
        DROP TRIGGER IF EXISTS update_cached_data_updated_at ON cached_data
        "#,
    )
    .execute(&mut *transaction)
    .await?;

    // Create the trigger
    sqlx::query(
        r#"
        CREATE TRIGGER update_cached_data_updated_at
            BEFORE UPDATE ON cached_data
            FOR EACH ROW
            EXECUTE FUNCTION update_updated_at_column()
        "#,
    )
    .execute(&mut *transaction)
    .await?;

    // Create the index
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_cached_data_updated_at ON cached_data(updated_at)
        "#,
    )
    .execute(&mut *transaction)
    .await?;

    // Commit the transaction
    transaction.commit().await?;
    
    println!("Successfully ran all migrations");
    Ok(())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load configuration
    dotenv::dotenv().ok();
    
    let config = Config {
        redis_url: std::env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1/".to_string()),
        database_url: std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost/cache".to_string()),
        memory_cache_ttl: std::env::var("MEMORY_CACHE_TTL")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .unwrap(),
        redis_cache_ttl: std::env::var("REDIS_CACHE_TTL")
            .unwrap_or_else(|_| "300".to_string())
            .parse()
            .unwrap(),
        max_memory_items: std::env::var("MAX_MEMORY_ITEMS")
            .unwrap_or_else(|_| "10000".to_string())
            .parse()
            .unwrap(),
    };

    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Create database pool with optimized settings
    let pool = PgPoolOptions::new()
        .max_connections(32)
        .min_connections(4)
        .acquire_timeout(Duration::from_secs(3))
        .idle_timeout(Duration::from_secs(60))
        .connect(&config.database_url)
        .await
        .expect("Failed to connect to database");

    // Run migrations
    run_migrations(&pool)
        .await
        .expect("Failed to run database migrations");

    println!("Database migrations completed successfully");

    // Initialize cache service
    let cache_service = CacheService::new(config)
        .await
        .expect("Failed to create cache service");

    println!("Starting HTTP server at http://0.0.0.0:8080");

    // Start HTTP server with optimized settings
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(cache_service.clone()))
            .wrap(actix_web::middleware::Compress::default())
            .service(
                web::scope("/cache")
                    .route("/health", web::get().to(health_check))
                    .route("/{key}", web::get().to(get_cached_data))
                    .route("/{key}", web::put().to(set_cached_data))
                    .route("/{key}", web::delete().to(delete_cached_data))
            )
    })
    .workers(num_cpus::get() * 2)  // Double the number of workers
    .backlog(10000)                // Increased from 2048
    .max_connections(50000)        // Increased from 5000
    .keep_alive(Duration::from_secs(30))
    .bind("0.0.0.0:8080")?
    .run()
    .await
}