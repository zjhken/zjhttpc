use std::time::Duration;
use zjhttpc::{client::ZJHttpClient, requestx::Request, Result};

fn main() -> Result<()> {
    async_std::task::block_on(async {
    println!("Testing proxy connect timeout functionality...");

    // Test 1: HTTP proxy with client-level connect timeout
    println!("\nTest 1: HTTP proxy with client-level connect timeout (1 second)");
    let client = ZJHttpClient::builder()
        .set_global_connect_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let mut req = Request::new("GET", "http://httpbin.org/get")?
        .set_proxy_from_url("http://192.0.2.1:8080")?; // RFC5737 test address, should be unreachable

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✗ Unexpected success in {:?}", elapsed),
        Err(e) => {
            println!("✓ Expected proxy connection failure: {:?}", e);
            println!("  Time elapsed: {:?}", elapsed);
            if elapsed > Duration::from_secs(2) {
                println!("  ⚠️  Timeout took longer than expected");
            } else {
                println!("  ✓ Timeout occurred within expected timeframe");
            }
        }
    }

    // Test 2: HTTPS proxy with request-level connect timeout
    println!("\nTest 2: HTTPS proxy with request-level connect timeout (2 seconds)");
    let client = ZJHttpClient::builder().build().unwrap(); // Default 3s timeout
    let mut req = Request::new("GET", "http://httpbin.org/get")?
        .set_proxy_from_url("https://192.0.2.1:8443")? // RFC5737 test address, should be unreachable
        .set_connect_timeout(Duration::from_secs(2));

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✗ Unexpected success in {:?}", elapsed),
        Err(e) => {
            println!("✓ Expected proxy connection failure: {:?}", e);
            println!("  Time elapsed: {:?}", elapsed);
            if elapsed > Duration::from_secs(3) {
                println!("  ⚠️  Timeout took longer than expected");
            } else {
                println!("  ✓ Timeout occurred within expected timeframe");
            }
        }
    }

    // Test 3: Request-level timeout overrides client-level timeout for proxy
    println!("\nTest 3: Request-level timeout (1s) overrides client-level timeout (5s) for proxy");
    let client = ZJHttpClient::builder()
        .set_global_connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut req = Request::new("GET", "http://httpbin.org/get")?
        .set_proxy_from_url("http://192.0.2.1:8080")? // RFC5737 test address, should be unreachable
        .set_connect_timeout(Duration::from_secs(1));

    let start = std::time::Instant::now();
    let result = client.send(&mut req).await;
    let elapsed = start.elapsed();

    match result {
        Ok(_response) => println!("✗ Unexpected success in {:?}", elapsed),
        Err(e) => {
            println!("✓ Expected proxy connection failure: {:?}", e);
            println!("  Time elapsed: {:?}", elapsed);
            if elapsed > Duration::from_secs(2) {
                println!("  ⚠️  Timeout took longer than expected (should be ~1s)");
            } else {
                println!("  ✓ Timeout occurred within expected timeframe (~1s)");
            }
        }
    }

    println!("\n=== Proxy Connect Timeout Tests Complete ===");

    Ok(())
    })
}