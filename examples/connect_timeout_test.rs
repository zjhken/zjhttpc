use std::time::Duration;
use zjhttpc::{client::ZJHttpClient, requestx::Request};

fn main() -> anyhow::Result<()> {
    async_std::task::block_on(async {
    // Test 1: Client-level connect timeout
    println!("Test 1: Client-level connect timeout (default 3 seconds)");
    let client = ZJHttpClient::new();
    let mut req = Request::new("GET", "http://httpbin.org/delay/1")?;

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✓ Request succeeded in {:?}", elapsed),
        Err(e) => println!("✗ Request failed: {:?}", e),
    }

    // Test 2: Client-level custom connect timeout
    println!("\nTest 2: Client-level custom connect timeout (1 second)");
    let client = ZJHttpClient::new().set_connect_timeout(Duration::from_secs(1));
    let mut req = Request::new("GET", "http://httpbin.org/delay/0.5")?;

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✓ Request succeeded in {:?}", elapsed),
        Err(e) => println!("✗ Request failed: {:?}", e),
    }

    // Test 3: Request-level connect timeout
    println!("\nTest 3: Request-level connect timeout (500ms)");
    let client = ZJHttpClient::new(); // Default 3s timeout
    let mut req = Request::new("GET", "http://httpbin.org/delay/0.2")?
        .set_connect_timeout(Duration::from_millis(500));

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✓ Request succeeded in {:?}", elapsed),
        Err(e) => println!("✗ Request failed: {:?}", e),
    }

    // Test 4: Request-level timeout overrides client-level timeout
    println!("\nTest 4: Request-level timeout (2s) overrides client-level timeout (5s)");
    let client = ZJHttpClient::new().set_connect_timeout(Duration::from_secs(5));
    let mut req = Request::new("GET", "http://httpbin.org/delay/1")?
        .set_connect_timeout(Duration::from_secs(2));

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✓ Request succeeded in {:?}", elapsed),
        Err(e) => println!("✗ Request failed: {:?}", e),
    }

    // Test 5: Timeout test with invalid/unreachable host
    println!("\nTest 5: Connect timeout with unreachable host (should timeout in 1s)");
    let client = ZJHttpClient::new().set_connect_timeout(Duration::from_secs(1));
    let mut req = Request::new("GET", "http://192.0.2.1:8080/")?; // RFC5737 test address, should be unreachable

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✗ Unexpected success in {:?}", elapsed),
        Err(e) => {
            println!("✓ Expected failure: {:?}", e);
            println!("  Time elapsed: {:?}", elapsed);
        }
    }

    Ok(())
    })
}