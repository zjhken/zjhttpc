use std::time::Instant;
use async_std::io::{ReadExt, WriteExt};
use async_std::net::{TcpListener, TcpStream};
use async_std::task;
use zjhttpc::client::ZJHttpClient;
use zjhttpc::methods;
use zjhttpc::requestx::Request;

/// Response style the mock server should send.
#[derive(Clone, Copy, Debug)]
enum RespStyle {
    /// `Content-Length` + `Connection: keep-alive`
    FixedLength,
    /// `Transfer-Encoding: chunked`
    Chunked,
    /// No length header at all — client must read until EOF
    NoLength,
    /// Explicit `Connection: close` after one response
    CloseAfter,
}

async fn run_server(listener: TcpListener, style: RespStyle) {
    let mut conn_no: u64 = 0;
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[server] accept failed: {e}");
                continue;
            }
        };
        conn_no += 1;
        eprintln!("[server] conn#{conn_no} accepted from {peer}");
        task::spawn(handle_conn(conn_no, stream, style));
    }
}

async fn handle_conn(conn_no: u64, mut stream: TcpStream, style: RespStyle) {
    loop {
        // Read request headers
        let mut header_buf: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = match stream.read(&mut byte).await {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("[server] conn#{conn_no} read header err: {e}");
                    return;
                }
            };
            if n == 0 {
                eprintln!("[server] conn#{conn_no} EOF during header");
                return;
            }
            header_buf.push(byte[0]);
            if header_buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }

        let header_str = String::from_utf8_lossy(&header_buf);
        let first_line = header_str.lines().next().unwrap_or("");
        let content_length: usize = header_str
            .lines()
            .find_map(|l| {
                let l = l.to_ascii_lowercase();
                l.strip_prefix("content-length: ")?.trim().parse().ok()
            })
            .unwrap_or(0);

        eprintln!(
            "[server] conn#{conn_no} req: {first_line}  (Content-Length={content_length})"
        );

        // Read body
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            if let Err(e) = stream.read_exact(&mut body).await {
                eprintln!("[server] conn#{conn_no} read body err: {e}");
                return;
            }
        }

        let resp_body = br#"{"ok":true,"echoed":true}"#;
        let (head, do_close) = match style {
            RespStyle::FixedLength => (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                    resp_body.len()
                ),
                false,
            ),
            RespStyle::Chunked => (
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n".to_string(),
                false,
            ),
            RespStyle::NoLength => (
                // No Content-Length / chunked — server must close the socket to signal body end
                "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n".to_string(),
                true,
            ),
            RespStyle::CloseAfter => (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                ),
                true,
            ),
        };

        if let Err(e) = stream.write_all(head.as_bytes()).await {
            eprintln!("[server] conn#{conn_no} write head err: {e}");
            return;
        }
        match style {
            RespStyle::Chunked => {
                let chunk = format!("{:x}\r\n", resp_body.len());
                stream.write_all(chunk.as_bytes()).await.ok();
                stream.write_all(resp_body).await.ok();
                stream.write_all(b"\r\n0\r\n\r\n").await.ok();
            }
            _ => {
                stream.write_all(resp_body).await.ok();
            }
        }
        let _ = stream.flush().await;
        eprintln!("[server] conn#{conn_no} sent response ({:?})", style);

        if do_close {
            eprintln!("[server] conn#{conn_no} closing after Connection: close");
            return;
        }
    }
}

async fn run_one(style: RespStyle) -> zjhttpc::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}/echo");
    eprintln!("\n[test] ===== {style:?} server at {url} =====");

    let server_task = task::spawn(run_server(listener, style));

    let client = ZJHttpClient::builder().build().unwrap();
    let body = r#"{"hello":"world","n":1}"#;

    for i in 0..15 {
        let t0 = Instant::now();
        let mut req = Request::new(methods::POST, &url)
            .unwrap()
            .set_body_string(body);
        let mut resp = match client.send(&mut req).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[test] iter {i}: send() FAILED: {e:#}");
                panic!("iter {i}: send() failed: {e:#}");
            }
        };
        let status = resp.status_code();
        let resp_body = match resp.body_string().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[test] iter {i}: body_string() FAILED status={status}: {e:#}");
                panic!("iter {i}: body_string() failed: {e:#}");
            }
        };
        eprintln!(
            "[test] iter {i}: status={status} body={resp_body} in {:?}",
            t0.elapsed()
        );
        assert_eq!(status, 200, "iter {i}: expected 200, got {status}");
        assert!(
            resp_body.contains(r#""ok":true"#),
            "iter {i}: body mismatch: {resp_body}"
        );
    }

    server_task.cancel().await;
    Ok(())
}

#[async_std::test]
#[tracing_test::traced_test]
async fn test_consecutive_calls_fixed_length() -> zjhttpc::Result<()> {
    run_one(RespStyle::FixedLength).await
}

#[async_std::test]
#[tracing_test::traced_test]
async fn test_consecutive_calls_chunked() -> zjhttpc::Result<()> {
    run_one(RespStyle::Chunked).await
}

#[async_std::test]
#[tracing_test::traced_test]
async fn test_consecutive_calls_no_length() -> zjhttpc::Result<()> {
    run_one(RespStyle::NoLength).await
}

#[async_std::test]
#[tracing_test::traced_test]
async fn test_consecutive_calls_close_after() -> zjhttpc::Result<()> {
    run_one(RespStyle::CloseAfter).await
}
