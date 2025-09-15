use anyhow_ext::{Context, Result, anyhow};
use async_std::{
    future::{self, timeout},
    io::{ReadExt, WriteExt},
    net::TcpStream,
};

use async_tls::{TlsConnector, client::TlsStream};
use dashmap::DashMap;
use derive_builder::Builder;
use nom::{
    IResult, Parser,
    bytes::complete::{is_not, tag, take_till},
};

use rustls_native_certs::load_native_certs;
use std::{
    net::SocketAddr,
    sync::{Arc, LazyLock},
    time::Duration,
};

use crate::{
    misc::{Body, TrustStorePem},
    requestx::Request,
    response::Response,
    stream::BoxedStream,
};
use tracing::{error, info, trace, warn};

// TODO: combine TCP pool with TLS pool
static TCP_POOL: LazyLock<DashMap<SocketAddr, Vec<BoxedStream>>> = LazyLock::new(DashMap::new);
static TLS_POOL: LazyLock<DashMap<SocketAddr, Vec<BoxedStream>>> = LazyLock::new(DashMap::new);

// TODO: default value with builder
#[derive(Builder, Default, Debug, Clone)]
#[builder(setter(strip_option))]
pub struct ZJHttpClient {
    // connection_pool: unimplemented!(),
    pub global_total_timeout: Duration,
    pub global_header_timeout: Duration,
    pub global_trust_store_pem: Option<TrustStorePem>,
}

impl ZJHttpClient {
    #[must_use]
    pub fn new() -> ZJHttpClient {
        ZJHttpClient {
            global_total_timeout: Duration::from_secs(300),
            global_header_timeout: Duration::from_secs(30),
            global_trust_store_pem: None,
        }
    }

    pub async fn send(&self, req: &mut Request) -> Result<Response> {
        let addr = resolve_1st_ip(req).await.dot()?;
        let mut stream: BoxedStream = pick_or_connect_stream(self, &req, &addr).await.dot()?;
        send_header(req, &mut stream).await.dot()?;
        send_body(req, &mut stream).await.dot()?;
        let resp = read_headers_to_resp(req, stream, addr).await.dot()?;
        return Ok(resp);
    }

    pub async fn send_header_only(&self, req: &mut Request) -> Result<(BoxedStream, SocketAddr)> {
        let addr = resolve_1st_ip(req).await.dot()?;
        let mut stream: BoxedStream = pick_or_connect_stream(self, &req, &addr).await.dot()?;
        send_header(req, &mut stream).await.dot()?;
        return Ok((stream, addr));
    }

    pub async fn send_body_only(
        &self,
        req: &mut Request,
        mut stream_to_write: BoxedStream,
        addr: SocketAddr,
    ) -> Result<Response> {
        send_body(req, &mut stream_to_write).await.dot()?;
        let resp = read_headers_to_resp(req, stream_to_write, addr)
            .await
            .dot()?;
        return Ok(resp);
    }
}

async fn pick_or_connect_stream(
    client: &ZJHttpClient,
    req: &Request,
    addr: &SocketAddr,
) -> Result<BoxedStream> {
    match req.url.scheme() {
        "http" => {
            if let Some(Some(mut stream_from_pool)) = TCP_POOL.get_mut(addr).map(|mut x| x.pop()) {
                if !is_stream_closed(&mut stream_from_pool).await {
                    trace!("picking up stream from pool");
                    return Ok(stream_from_pool);
                } else {
                    info!(?addr, "stream was picked but it is closed");
                }
            } else {
                trace!(?addr, "no existing connection for this addr")
            }
            let tcp_stream = TcpStream::connect(&addr).await.dot().unwrap();
            return Ok(Box::new(tcp_stream));
        }
        "https" => {
            if let Some(Some(mut stream_from_pool)) = TLS_POOL.get_mut(addr).map(|mut x| x.pop()) {
                if !is_stream_closed(&mut stream_from_pool).await {
                    info!(?addr, "picking up stream from pool");
                    return Ok(stream_from_pool);
                } else {
                    info!(?addr, "stream was picked but it is closed");
                }
            } else {
                trace!(?addr, "no existing connection for this addr")
            }
            let tls_config = create_tls_config(&client.global_trust_store_pem).dot()?;
            let tls_config = Arc::new(tls_config);
            let tls_connector: TlsConnector = tls_config.into();
            let host = if let url::Host::Domain(s) =
                req.url.host().ok_or(anyhow!("no host in URL")).dot()?
            {
                s
            } else {
                return Err(anyhow!(
                    "HTTPS request should specify the Domain instead of IP, or you can provide the sni doman name"
                ));
            };
            let tcp_stream = TcpStream::connect(addr).await.dot()?;
            let tls_stream = tls_connector.connect(host, tcp_stream).await.dot()?;
            return Ok(Box::new(tls_stream));
        }
        others => return Err(anyhow!("scheme {others} is not supported at the moment")),
    }
}

async fn is_stream_closed(stream: &mut BoxedStream) -> bool {
    if let Some(stream) = stream.as_any_mut().downcast_mut::<TlsStream<TcpStream>>() {
        return is_stream_closed_inner(stream.get_mut()).await;
    } else if let Some(stream) = stream.as_any_mut().downcast_mut::<TcpStream>() {
        return is_stream_closed_inner(stream).await;
    } else {
        warn!("downcast failed");
        return true;
    }

    async fn is_stream_closed_inner(tcp: &mut TcpStream) -> bool {
        let mut buf = [0u8; 1];
        let result = timeout(Duration::from_secs(1), tcp.peek(&mut buf)).await;
        match result {
            Ok(result) => match result {
                Ok(0) => {
                    info!("read 0, stream is closed");
                    return true;
                }
                Ok(n) => {
                    info!("read {n}, stream is still open, but strange");
                    return false;
                }
                Err(err) => {
                    info!("get unexpected error, stream closed");
                    return true;
                }
            },
            Err(_e) => {
                trace!("timeout, stream is still open");
                return false;
            }
        }
    }
}

async fn resolve_1st_ip(req: &mut Request) -> Result<SocketAddr> {
    let mut addrs = req.url.socket_addrs(|| None).dot()?;
    let addr = addrs
        .pop()
        .ok_or_else(|| anyhow!("no result in DNS resolve"))
        .dot()?;
    return Ok(addr);
}

pub fn create_tls_config(trust_store: &Option<TrustStorePem>) -> Result<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    let certs = match trust_store {
        None => load_native_certs().expect("failed to load system certs"),
        Some(TrustStorePem::Bytes(data)) => {
            let mut reader = std::io::BufReader::new(data.as_slice());
            rustls_pemfile::certs(&mut reader)
                .filter_map(|re| match re {
                    Ok(c) => Some(c),
                    Err(err) => {
                        error!(?err, "failed to parse cert");
                        None
                    }
                })
                .collect::<Vec<_>>()
        }
        Some(TrustStorePem::Path(p)) => {
            let file = std::fs::File::open(p)
                .dot()
                .context("failed to open trust store file")?;
            let mut reader = std::io::BufReader::new(file);
            rustls_pemfile::certs(&mut reader)
                .filter_map(|re| match re {
                    Ok(c) => Some(c),
                    Err(err) => {
                        error!(?err, "failed to parse cert");
                        None
                    }
                })
                .collect::<Vec<_>>()
        }
    };
    for cert in certs {
        root_store.add(&rustls::Certificate(cert.to_vec())).dot()?;
    }
    let client_config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    return Ok(client_config);
}

async fn send_header<S>(req: &Request, stream: &mut S) -> Result<()>
where
    S: async_std::io::Read + async_std::io::Write + Unpin + Send + Sync + 'static,
{
    stream.write_all(req.method.as_bytes()).await.dot()?;
    stream.write_all(b" ").await.dot()?;
    let path = req.url.path();
    stream.write_all(path.as_bytes()).await.dot()?;
    if let Some(q) = req.url.query() {
        stream.write_all(b"?").await.dot()?;
        stream.write_all(q.as_bytes()).await.dot()?;
    }
    // TODO: maybe need to handle segements like "#a=b"
    stream.write_all(b" ").await.dot()?;
    stream.write_all(b"HTTP/1.1\r\n").await.dot()?;
    // insert headers
    for (key, values) in &req.headers {
        stream.write_all(key.as_bytes()).await.dot()?;
        stream.write_all(b": ").await.dot()?;
        // TODO: handle multi same key headers
        stream
            .write_all(values.first().unwrap().as_bytes())
            .await
            .dot()?;
        stream.write_all(b"\r\n").await.dot()?;
    }
    stream.write_all(b"Content-Length: ").await.dot()?;
    stream
        .write_all(req.content_length.to_string().as_bytes())
        .await
        .dot()?;
    stream.write_all(b"\r\n").await.dot()?;
    if let Some((username, password)) = &req.basic_auth {
        let encoded = base64_simd::STANDARD.encode_to_string(format!("{username}:{password}"));
        let s = format!("Authorization: Basic {encoded}\r\n");
        stream.write_all(s.as_bytes()).await.dot()?;
    }

    if req.expect_continue {
        stream.write_all(b"Expect: 100-continue\r\n").await.dot()?;
    }

    stream
        .write_all(b"Connection: keep-alive\r\n")
        .await
        .dot()?;
    stream.write_all(b"\r\n").await.dot()?;
    stream.flush().await.dot()?;

    if req.expect_continue {
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.dot()?;
        if n == 0 {
            return Err(anyhow!(
                "stream closed before read the 100 continue response"
            ));
        }
        let resp = std::str::from_utf8(&buf[0..n])
            .dot()
            .context("resp after expect 100 is not utf8")?;
        if resp != "HTTP/1.1 100 Continue\r\n\r\n" {
            return Err(anyhow!("received non-100-continue resp={resp}"));
        }
    }
    Ok(())
}

async fn send_body<S>(req: &mut Request, stream_to_write: &mut S) -> Result<()>
where
    S: async_std::io::Read + async_std::io::Write + Unpin + Send + Sync + 'static,
{
    match &mut req.body {
        Body::None => return Ok(()),
        Body::Stream(stream_to_read) => {
            let len = req.content_length as usize;
            let mut buf = vec![0u8; 1024 * 128]; // 128KB
            let mut read_n = 0usize;
            loop {
                let n = stream_to_read.read(&mut buf).await.dot()?;
                if n == 0 {
                    trace!(n, "read stream ended");
                    break;
                }
                read_n += n;
                stream_to_write.write_all(&buf[..n]).await.dot()?;
                if read_n == len {
                    trace!("sent enough bytes");
                    break;
                }
            }
        }
        Body::Str(s) => {
            stream_to_write.write_all(s.as_bytes()).await.dot()?;
        }
        Body::Form => unimplemented!(),
        Body::ByteSlice => unimplemented!(),
    }
    Ok(())
}

async fn read_headers_to_resp(
    req: &mut Request,
    mut stream: BoxedStream,
    addr: SocketAddr,
) -> Result<Response> {
    // let mut buf = [0u8; 1024 * 8];
    let data = {
        let fut = read_until(&mut stream, b"\r\n");
        if let Some(dur) = req.header_timeout {
            future::timeout(dur, fut).await.dot()??
        } else {
            fut.await.dot()?
        }
    };
    let input = std::str::from_utf8(data.as_ref()).dot()?;
    info!(input);
    let (_, (_, http_version, _, status_code, _)) = parse_resp_first_line(input)
        .map_err(|e| {
            anyhow!(
                "{err}:parse resp first line failed. firstLine={line}",
                err = e.to_owned(),
                line = input.to_string()
            )
        })
        .dot()?;
    let input = read_until(&mut stream, b"\r\n\r\n").await.dot()?;
    let input = std::str::from_utf8(input.as_ref()).dot()?;
    let headers = parse_headers(input)
        .dot()?
        .into_iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), value.to_owned()))
        .collect::<Vec<_>>();
    return Response::new_from_parse_result(
        http_version,
        status_code,
        headers,
        stream,
        req.url.scheme() == "https",
        addr,
    )
    .map_err(|e| anyhow!("{e}"));
}

fn parse_headers(input: &str) -> Result<Vec<(&str, &str)>> {
    let mut vec = vec![];
    let mut rest: &str = input;
    loop {
        let (out, (key, _, value, _)) = parse_one_line_header(rest)
            .map_err(|e| {
                anyhow!(
                    "{err}:failed to parse one line header. line={line}",
                    err = e.to_owned(),
                    line = input.to_string()
                )
            })
            .dot()?;
        rest = out;
        vec.push((key, value));
        if rest == "\r\n" {
            break;
        }
    }
    Ok(vec)
}

fn parse_one_line_header(input: &str) -> IResult<&str, (&str, &str, &str, &str)> {
    (is_not(": "), tag(": "), take_till(|x| x == '\r' || x == '\n'), tag("\r\n")).parse(input)
}

fn parse_resp_first_line(input: &str) -> IResult<&str, (&str, &str, &str, &str, &str)> {
    (
        tag("HTTP/"),
        take_till(|x| x == ' '),
        tag(" "),
        take_till(|x| x == ' ' || x == '\r'), // status message is not mandortory
        take_till(|x| x == '\n')
    )
        .parse(input)
}

// TODO: use nom to parse stream
pub async fn read_until<S>(stream: &mut S, delimiter: &[u8]) -> Result<Vec<u8>>
where
    S: async_std::io::Read + Unpin + Send + Sync + 'static,
{
    let mut buf = Vec::new();
    let mut one_byte = [0u8; 1];
    if delimiter.is_empty() {
        return Ok(buf);
    }
    loop {
        let read_n = stream.read(&mut one_byte).await.dot()?;
        if read_n == 0 {
            break;
        }
        buf.push(one_byte[0]);
        if buf.ends_with(delimiter) {
            break;
        }
    }
    Ok(buf)
}

pub fn return_stream_to_pool(resp: &mut Response) {
    if !resp.body_readed {
        // TODO: for now just close the connection
        // in the future we can try to drain it with timeout
        // but during the data reading, we have to consider the content-length and transfer-encoding
        return;
    }
    if let Some(stream) = resp.body_stream.take() {
        // TODO: cast the stream to known which type, so no need the is_tls, just put it back to pool
        if resp.is_tls {
            if let Some(mut pool) = TLS_POOL.get_mut(&resp.addr) {
                let len = pool.len();
                // TODO: allow user to set the pool size
                if len <= 30 {
                    pool.push(stream);
                    let len = pool.len();
                    trace!(len, "tls stream returned");
                } else {
                    trace!(len, "tls pool is full");
                }
            } else {
                TLS_POOL.insert(resp.addr, vec![stream]);
                trace!("add new vec to tls pool");
            }
        } else if let Some(mut pool) = TCP_POOL.get_mut(&resp.addr) {
            let len = pool.len();
            if len <= 30 {
                pool.push(stream);
                let len = pool.len();
                trace!(len, "tcp stream returned");
            } else {
                trace!(len, "tcp pool is full");
            }
        } else {
            TCP_POOL.insert(resp.addr, vec![stream]);
            trace!("tcp add new vec to pool");
        }
    }
}

pub enum HttpVersion {
    V1_1,
    V1_0,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_one_line_header_basic() {
        let input = "Content-Type: application/json\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_ok());
        
        let (remaining, (key, colon_space, value, crlf)) = result.unwrap();
        assert_eq!(key, "Content-Type");
        assert_eq!(colon_space, ": ");
        assert_eq!(value, "application/json");
        assert_eq!(crlf, "\r\n");
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_parse_one_line_header_with_spaces_in_value() {
        let input = "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64)\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_ok());
        
        let (remaining, (key, colon_space, value, crlf)) = result.unwrap();
        assert_eq!(key, "User-Agent");
        assert_eq!(colon_space, ": ");
        assert_eq!(value, "Mozilla/5.0 (Windows NT 10.0; Win64; x64)");
        assert_eq!(crlf, "\r\n");
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_parse_one_line_header_empty_value_with_space() {
        let input = "X-Custom-Header: \r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_ok());
        
        let (remaining, (key, colon_space, value, crlf)) = result.unwrap();
        assert_eq!(key, "X-Custom-Header");
        assert_eq!(colon_space, ": ");
        assert_eq!(value, "");
        assert_eq!(crlf, "\r\n");
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_parse_one_line_header_empty_value_no_space() {
        let input = "X-Custom-Header:\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_one_line_header_with_remaining_input() {
        let input = "Host: example.com\r\nContent-Length: 123\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_ok());
        
        let (remaining, (key, colon_space, value, crlf)) = result.unwrap();
        assert_eq!(key, "Host");
        assert_eq!(colon_space, ": ");
        assert_eq!(value, "example.com");
        assert_eq!(crlf, "\r\n");
        assert_eq!(remaining, "Content-Length: 123\r\n");
    }

    #[test]
    fn test_parse_one_line_header_special_characters() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_ok());
        
        let (remaining, (key, colon_space, value, crlf)) = result.unwrap();
        assert_eq!(key, "Authorization");
        assert_eq!(colon_space, ": ");
        assert_eq!(value, "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9");
        assert_eq!(crlf, "\r\n");
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_parse_one_line_header_numbers_and_symbols() {
        let input = "Content-Length: 1024\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_ok());
        
        let (remaining, (key, colon_space, value, crlf)) = result.unwrap();
        assert_eq!(key, "Content-Length");
        assert_eq!(colon_space, ": ");
        assert_eq!(value, "1024");
        assert_eq!(crlf, "\r\n");
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_parse_one_line_header_missing_colon() {
        let input = "InvalidHeader application/json\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_one_line_header_missing_crlf() {
        let input = "Content-Type: application/json";
        let result = parse_one_line_header(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_one_line_header_only_crlf() {
        let input = "\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_one_line_header_empty_string() {
        let input = "";
        let result = parse_one_line_header(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_one_line_header_case_sensitive() {
        let input = "content-type: text/html\r\n";
        let result = parse_one_line_header(input);
        assert!(result.is_ok());
        
        let (remaining, (key, colon_space, value, crlf)) = result.unwrap();
        assert_eq!(key, "content-type");
        assert_eq!(colon_space, ": ");
        assert_eq!(value, "text/html");
        assert_eq!(crlf, "\r\n");
        assert_eq!(remaining, "");
    }
}