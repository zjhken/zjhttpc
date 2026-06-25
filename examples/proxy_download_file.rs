use std::time::Duration;

use async_std::{fs::File, io::ReadExt, io::WriteExt, path::Path};

use zjhttpc::{Result, client::ZJHttpClient, error::ZjhttpcError, requestx::Request};

const URL: &str = "https://raw.githubusercontent.com/metowolf/iplist/master/data/country/CN.txt";
const PROXY: &str = "http://10.0.0.2:1080";
const OUTPUT: &str = "CN.txt";

fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.2} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

fn main() -> Result<()> {
    async_std::task::block_on(async {
        println!("Downloading {URL}");
        println!("via proxy {PROXY}");

        let client = ZJHttpClient::builder()
            .set_global_connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap();

        let mut req = Request::new("GET", URL)?.set_proxy_from_url(PROXY)?;

        let start = std::time::Instant::now();
        let mut response = client.send(&mut req).await?;
        println!(
            "Status: {} (in {:?})",
            response.status_code(),
            start.elapsed()
        );

        if !response.is_success() {
            let body = response.body_string().await.unwrap_or_default();
            return Err(ZjhttpcError::InvalidResponse(format!(
                "download failed with status {}: {}",
                response.status_code(),
                body.chars().take(500).collect::<String>()
            ))
            .into());
        }

        let output_path = Path::new(OUTPUT);
        let mut file = File::create(output_path).await?;

        let mut total: u64 = 0;
        if let Some(mut stream) = response.body_managed_stream() {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = stream.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n]).await?;
                total += n as u64;
            }
        }
        file.flush().await?;

        println!(
            "Saved to {} ({}) in {:?}",
            output_path.display(),
            format_bytes(total),
            start.elapsed()
        );

        Ok(())
    })
}
