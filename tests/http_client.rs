use anyhow::Result;
use zjhttpc::client::ZJHttpClient;
use zjhttpc::methods;
use zjhttpc::requestx::Request;

#[async_std::test]
async fn test_send_get_to_datacenter() -> Result<()> {
    let client = ZJHttpClient::builder().build().unwrap();
    let mut req = Request::new(methods::GET, "https://www.baidu.com").unwrap();

    let mut resp = client.send(&mut req).await?;
    assert!(
        resp.is_success(),
        "expected 2xx status, got {}",
        resp.status_code()
    );

    let body = resp.body_string().await?;
    assert!(
        body.contains("location.href.replace") || body.contains("refresh"),
        "response body did not contain expected field",
    );

    Ok(())
}
