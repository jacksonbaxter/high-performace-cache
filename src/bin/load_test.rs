use tokio;
use std::time::{Instant, Duration};
use futures::future::join_all;
use std::sync::Arc;

#[derive(Debug)]
struct TestResults {
    #[allow(dead_code)]
    operation: String,
    requests: usize,
    failures: usize,
    total_time: Duration,
    avg_latency: Duration,
    requests_per_second: f64,
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

#[derive(Clone, Debug)]
enum TestOperation {
    Read,
    Write,
}

async fn run_load_test(operation: TestOperation, num_requests: usize, concurrent_requests: usize) -> TestResults {
    let client = Arc::new(reqwest::Client::builder()
        .pool_max_idle_per_host(0) // Close connections immediately
        .pool_idle_timeout(Some(Duration::from_secs(5)))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap());
    let url = "http://localhost:8080";
    let base_value = "test_value_that_is_long_enough_to_be_meaningful_for_testing";
    
    let start = Instant::now();
    let mut failures = 0;
    let mut successful_durations = Vec::new();
    
    // Process requests in batches
    for batch_start in (0..num_requests).step_by(concurrent_requests) {
        let batch_size = (batch_start + concurrent_requests).min(num_requests) - batch_start;
        let mut handles = Vec::with_capacity(batch_size);
        
        for i in batch_start..batch_start + batch_size {
            let client = client.clone();
            let key = format!("test_key_{}", i % concurrent_requests);
            let value = format!("{}_{}", base_value, i);
            let op = operation.clone();
            
            let handle = tokio::spawn(async move {
                match op {
                    TestOperation::Write => write_test(&client, url, &key, &value).await,
                    TestOperation::Read => read_test(&client, url, &key).await,
                }
            });
            
            handles.push(handle);
        }
        
        // Wait for batch completion
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
        
        // Add a small delay between batches
        tokio::time::sleep(Duration::from_millis(10)).await;
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
        operation: format!("{:?}", operation),
        requests: successful_requests,
        failures,
        total_time,
        avg_latency,
        requests_per_second,
    }
}

#[tokio::main]
async fn main() {
    // Use smaller numbers for initial test
    println!("\nPopulating initial data...");
    let initial_results = run_load_test(TestOperation::Write, 50, 5).await;
    println!("Initial population results: {:?}", initial_results);

    // Use more moderate numbers for the main test
    let num_requests = 5000;  // Reduced from 5000
    let concurrent_requests = 50;  // Reduced from 50
    
    // Run write test
    println!("\nRunning write test...");
    let write_results = run_load_test(TestOperation::Write, num_requests, concurrent_requests).await;
    
    println!("\nWrite Test Results:");
    println!("Total Requests: {}", write_results.requests);
    println!("Failed Requests: {}", write_results.failures);
    println!("Total Time: {:.2?}", write_results.total_time);
    println!("Average Latency: {:.2?}", write_results.avg_latency);
    println!("Requests/second: {:.2}", write_results.requests_per_second);
    
    // Run read test
    println!("\nRunning read test...");
    let read_results = run_load_test(TestOperation::Read, num_requests, concurrent_requests).await;
    
    println!("\nRead Test Results:");
    println!("Total Requests: {}", read_results.requests);
    println!("Failed Requests: {}", read_results.failures);
    println!("Total Time: {:.2?}", read_results.total_time);
    println!("Average Latency: {:.2?}", read_results.avg_latency);
    println!("Requests/second: {:.2}", read_results.requests_per_second);
}