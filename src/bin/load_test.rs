use tokio;
use std::time::{Instant, Duration};
use futures::future::join_all;
use std::sync::Arc;
use reqwest;

#[derive(Debug)]
struct TestResults {
    requests: usize,
    failures: usize,
    total_time: Duration,
    avg_latency: Duration,
    requests_per_second: f64,
}

#[derive(Clone, Debug)]
enum TestOperation {
    Read,
    Write,
}

async fn write_test(client: &reqwest::Client, url: &str, key: &str, value: &str) -> Result<Duration, reqwest::Error> {
    let start = Instant::now();
    let response = client.put(&format!("{}/cache/{}", url, key))
        .body(value.to_string())
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    
    response.error_for_status()?;
    Ok(start.elapsed())
}

async fn read_test(client: &reqwest::Client, url: &str, key: &str) -> Result<Duration, reqwest::Error> {
    let start = Instant::now();
    let response = client.get(&format!("{}/cache/{}", url, key))
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    
    response.error_for_status()?;
    Ok(start.elapsed())
}

async fn run_load_test(operation: TestOperation, num_requests: usize, concurrent_requests: usize) -> TestResults {
    let client = Arc::new(reqwest::Client::builder()
        .pool_max_idle_per_host(200)
        .pool_idle_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(10))
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_nodelay(true)
        .build()
        .unwrap());
    
    let url = std::env::var("SERVER_URL")
        .unwrap_or_else(|_| "http://cache-server:8080".to_string());
    
    let base_value = "test";
    let start = Instant::now();
    let mut failures = 0;
    let mut successful_durations = Vec::new();
    
    // Process requests in very large batches with controlled concurrency
    let batch_size = 1000;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrent_requests));
    
    for batch_start in (0..num_requests).step_by(batch_size) {
        let current_batch_size = (batch_start + batch_size).min(num_requests) - batch_start;
        let mut handles = Vec::with_capacity(current_batch_size);
        
        for i in batch_start..batch_start + current_batch_size {
            let client = client.clone();
            let key = format!("test_key_{}", i % concurrent_requests);
            let value = format!("{}_{}", base_value, i);
            let url = url.clone();
            let op = operation.clone();
            let semaphore = semaphore.clone();
            
            let handle = tokio::spawn(async move {
                let _permit = semaphore.acquire().await.unwrap();
                match op {
                    TestOperation::Write => write_test(&client, &url, &key, &value).await,
                    TestOperation::Read => read_test(&client, &url, &key).await,
                }
            });
            
            handles.push(handle);
        }
        
        let results = join_all(handles).await;
        for result in results {
            match result {
                Ok(Ok(duration)) => successful_durations.push(duration),
                Ok(Err(e)) => {
                    failures += 1;
                    eprintln!("Request failed: {}", e);
                },
                Err(e) => {
                    failures += 1;
                    eprintln!("Task failed: {}", e);
                }
            }
        }
        
        // Shorter delay between batches
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    let total_time = start.elapsed();
    let successful_requests = successful_durations.len();
    let avg_latency = if successful_requests > 0 {
        Duration::from_nanos(
            (successful_durations.iter().map(|d| d.as_nanos()).sum::<u128>() / successful_requests as u128) as u64
        )
    } else {
        Duration::from_secs(0)
    };
    
    let requests_per_second = successful_requests as f64 / total_time.as_secs_f64();
    
    TestResults {
        requests: successful_requests,
        failures,
        total_time,
        avg_latency,
        requests_per_second,
    }
}

#[tokio::main]
async fn main() {
    // Initial population with more data
    println!("\nPopulating initial data...");
    let initial_results = run_load_test(TestOperation::Write, 2000, 500).await;
    println!("Initial population results: {:?}", initial_results);

    // Even larger test with higher concurrency
    let num_requests = 20000;
    let concurrent_requests = 1000;
    
    // Run write test
    println!("\nRunning write test...");
    let write_results = run_load_test(TestOperation::Write, num_requests, concurrent_requests).await;
    print_results("Write", &write_results);
    
    // Run read test
    println!("\nRunning read test...");
    let read_results = run_load_test(TestOperation::Read, num_requests, concurrent_requests).await;
    print_results("Read", &read_results);
}

fn print_results(operation: &str, results: &TestResults) {
    println!("\n{} Test Results:", operation);
    println!("Total Requests: {}", results.requests);
    println!("Failed Requests: {}", results.failures);
    println!("Total Time: {:.2?}", results.total_time);
    println!("Average Latency: {:.2?}", results.avg_latency);
    println!("Requests/second: {:.2}", results.requests_per_second);
    println!("Success Rate: {:.2}%", 
        (results.requests as f64 / (results.requests + results.failures) as f64) * 100.0);
}