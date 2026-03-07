use zjhttpc::client::ZJHttpClient;
use zjhttpc::requestx::Request;

#[async_std::main]
async fn main() -> anyhow::Result<()> {
    // Create a client
    let client = ZJHttpClient::builder().build()?;

    // First request - login to get cookies
    let mut login_request = Request::new("POST", "https://httpbin.org/cookies/set?sessionid=abc123&userdata=alice")?
        .set_header("Accept", "application/json");

    let login_response = client.send(&mut login_request).await?;

    println!("Login response status: {}", login_response.status_code());

    // Read cookies from response
    let cookies = login_response.read_cookies();
    println!("Received {} cookies:", cookies.len());
    for cookie in &cookies {
        println!("  - {}={}", cookie.name, cookie.value);
    }

    // Second request - use cookies in the next request
    if !cookies.is_empty() {
        let mut dashboard_request = Request::new("GET", "https://httpbin.org/cookies")?
            .set_cookie(&cookies)
            .set_header("Accept", "application/json");

        let mut dashboard_response = client.send(&mut dashboard_request).await?;

        println!("Dashboard response status: {}", dashboard_response.status_code());

        // Read the response body to verify cookies were sent
        let body = dashboard_response.body_string().await?;
        println!("Response body: {}", body);
    }

    Ok(())
}
