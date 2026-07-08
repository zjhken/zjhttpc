use zjhttpc::client::ZJHttpClient;
use zjhttpc::requestx::Request;
use zjhttpc::Result;

/// Subscribe to Wikimedia's recent-changes SSE feed and print a few events.
#[async_std::main]
async fn main() -> Result<()> {
    let client = ZJHttpClient::builder().build().unwrap();

    let mut req = Request::new(
        "GET",
        "https://stream.wikimedia.org/v2/stream/recentchange",
    )?
    .set_header("Accept", "text/event-stream");

    let mut resp = client.send(&mut req).await?;
    println!("status: {}", resp.status_code());

    let mut sse = match resp.body_sse_stream() {
        Some(s) => s,
        None => {
            println!("no body stream");
            return Ok(());
        }
    };

    let mut count = 0;
    while let Some(ev) = sse.next_event().await? {
        count += 1;
        let kind = ev.event.as_deref().unwrap_or("message");
        let data = ev.data.trim_end_matches('\n');
        println!("#{count} [{kind}] id={:?} retry={:?} data={data}", ev.id, ev.retry);
        if count >= 5 {
            break;
        }
    }

    Ok(())
}
