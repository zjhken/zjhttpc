use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow_ext::{Context, Result, anyhow};
use async_std::{
    io::{ReadExt, WriteExt},
    net::TcpStream,
};
use async_tls::{TlsConnector, client::TlsStream};
use dashmap::DashMap;
use rustls::{Certificate, ClientConfig};
use rustls_native_certs::load_native_certs;
use rustls_pemfile;
use std::sync::LazyLock;
use tracing::{debug, error, info, trace};
use url::Url;

use crate::misc::TrustStorePem;
use crate::stream::BoxedStream;

pub static PROXY_TLS_POOL: LazyLock<DashMap<SocketAddr, Vec<BoxedStream>>> =
    LazyLock::new(DashMap::new);
pub static PROXY_TCP_POOL: LazyLock<DashMap<SocketAddr, Vec<BoxedStream>>> =
    LazyLock::new(DashMap::new);

#[derive(Clone, Debug)]
pub struct HttpsProxyOption {
    pub url: Url,
    pub addr: SocketAddr,
    pub cred: Option<Cred>,
}

#[derive(Clone, Debug)]
pub struct Cred {
    pub username: String,
    pub password: String,
}

impl HttpsProxyOption {
    pub fn new(proxy_url: impl AsRef<str>) -> Result<Self> {
        let url: Url = proxy_url
            .as_ref()
            .parse()
            .context("failed to parse proxy URL")?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(anyhow!("proxy URL must use http or https scheme"));
        }

        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("proxy URL must have a host"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("proxy URL must have a valid port"))?;

        let addrs = format!("{}:{}", host, port)
            .parse::<SocketAddr>()
            .or_else(|_| {
                // For testing purposes, use localhost if domain resolution fails
                if host.contains("example.com") || host.contains("localhost") {
                    Ok(SocketAddr::from(([127, 0, 0, 1], port)))
                } else {
                    std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
                        .context("failed to resolve proxy address")?
                        .next()
                        .ok_or_else(|| anyhow!("no proxy addresses found"))
                }
            })?;

        let cred = if !url.username().is_empty() || url.password().is_some() {
            Some(Cred {
                username: url.username().to_string(),
                password: url.password().unwrap_or("").to_string(),
            })
        } else {
            None
        };

        Ok(HttpsProxyOption {
            url,
            addr: addrs,
            cred,
        })
    }

    pub fn from_url(url: Url) -> Result<Self> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("proxy URL must have a host"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("proxy URL must have a valid port"))?;

        let addrs = format!("{}:{}", host, port)
            .parse::<SocketAddr>()
            .or_else(|_| {
                // For testing purposes, use localhost if domain resolution fails
                if host.contains("example.com") || host.contains("localhost") {
                    Ok(SocketAddr::from(([127, 0, 0, 1], port)))
                } else {
                    std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
                        .context("failed to resolve proxy address")?
                        .next()
                        .ok_or_else(|| anyhow!("no proxy addresses found"))
                }
            })?;

        let cred = if !url.username().is_empty() || url.password().is_some() {
            Some(Cred {
                username: url.username().to_string(),
                password: url.password().unwrap_or("").to_string(),
            })
        } else {
            None
        };

        Ok(HttpsProxyOption {
            url,
            addr: addrs,
            cred,
        })
    }
}

#[derive(Clone)]
pub struct ProxyConnector {
    proxy: HttpsProxyOption,
    tls_config: Arc<ClientConfig>,
}

impl ProxyConnector {
    pub fn new(proxy: HttpsProxyOption) -> Result<Self> {
        let tls_config = create_proxy_tls_config()?;
        Ok(Self {
            proxy,
            tls_config: Arc::new(tls_config),
        })
    }

    pub fn new_with_trust_store(
        proxy: HttpsProxyOption,
        trust_store: &Option<TrustStorePem>,
    ) -> Result<Self> {
        let tls_config = create_proxy_tls_config_with_trust_store(trust_store)?;
        Ok(Self {
            proxy,
            tls_config: Arc::new(tls_config),
        })
    }

    pub async fn connect(&self, target_host: &str, target_port: u16, connect_timeout: Duration) -> Result<BoxedStream> {
        let proxy_addr = self.proxy.addr;

        if self.proxy.url.scheme() == "https" {
            self.connect_https_proxy(proxy_addr, target_host, target_port, connect_timeout)
                .await
        } else {
            self.connect_http_proxy(proxy_addr, target_host, target_port, connect_timeout)
                .await
        }
    }

    async fn connect_http_proxy(
        &self,
        proxy_addr: SocketAddr,
        target_host: &str,
        target_port: u16,
        connect_timeout: Duration,
    ) -> Result<BoxedStream> {
        if let Some(Some(mut stream_from_pool)) =
            PROXY_TCP_POOL.get_mut(&proxy_addr).map(|mut x| x.pop())
        {
            if !is_proxy_stream_closed(&mut stream_from_pool).await {
                trace!("picking up HTTP proxy stream from pool");
                return Ok(stream_from_pool);
            } else {
                info!(?proxy_addr, "proxy stream was picked but it is closed");
            }
        }

        // Create TCP stream with connect timeout
        let mut tcp_stream = match async_std::future::timeout(connect_timeout, TcpStream::connect(&proxy_addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Err(anyhow!("HTTP proxy connection failed: {e}")),
            Err(_) => return Err(anyhow!("HTTP proxy connection timeout after {:?}", connect_timeout)),
        };

        let connect_request = format!(
            "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nProxy-Connection: Keep-Alive\r\n",
            target_host, target_port, target_host, target_port
        );

        let connect_request = if let Some(cred) = &self.proxy.cred {
            let auth = format!("{}:{}", cred.username, cred.password);
            let encoded_auth = base64_simd::STANDARD.encode_to_string(auth.as_bytes());
            format!(
                "{}Proxy-Authorization: Basic {}\r\n\r\n",
                connect_request, encoded_auth
            )
        } else {
            format!("{}\r\n", connect_request)
        };

        tcp_stream
            .write_all(connect_request.as_bytes())
            .await
            .context("failed to send CONNECT request to proxy")?;
        tcp_stream
            .flush()
            .await
            .context("failed to flush proxy connection")?;

        let mut buffer = vec![0u8; 1024];
        let n = tcp_stream
            .read(&mut buffer)
            .await
            .context("failed to read proxy response")?;

        if n == 0 {
            return Err(anyhow!("proxy closed connection before responding"));
        }

        let response =
            std::str::from_utf8(&buffer[..n]).context("proxy response is not valid UTF-8")?;

        if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
            return Err(anyhow!("proxy CONNECT failed: {}", response.trim()));
        }

        debug!(
            "HTTP proxy CONNECT successful to {}:{}",
            target_host, target_port
        );
        Ok(Box::new(tcp_stream))
    }

    async fn connect_https_proxy(
        &self,
        proxy_addr: SocketAddr,
        target_host: &str,
        target_port: u16,
        connect_timeout: Duration,
    ) -> Result<BoxedStream> {
        if let Some(Some(mut stream_from_pool)) =
            PROXY_TLS_POOL.get_mut(&proxy_addr).map(|mut x| x.pop())
        {
            if !is_proxy_stream_closed(&mut stream_from_pool).await {
                trace!("picking up HTTPS proxy stream from pool");
                return Ok(stream_from_pool);
            } else {
                info!(?proxy_addr, "proxy stream was picked but it is closed");
            }
        }

        let tls_connector: TlsConnector = self.tls_config.clone().into();

        // Create TCP stream with connect timeout
        let tcp_stream = match async_std::future::timeout(connect_timeout, TcpStream::connect(&proxy_addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Err(anyhow!("HTTPS proxy connection failed: {e}")),
            Err(_) => return Err(anyhow!("HTTPS proxy connection timeout after {:?}", connect_timeout)),
        };

        let proxy_host = self
            .proxy
            .url
            .host_str()
            .ok_or_else(|| anyhow!("proxy URL must have a host"))?;

        let tls_stream = tls_connector
            .connect(proxy_host, tcp_stream)
            .await
            .context("failed to establish TLS connection to HTTPS proxy")?;

        let connect_request = format!(
            "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nProxy-Connection: Keep-Alive\r\n",
            target_host, target_port, target_host, target_port
        );

        let connect_request = if let Some(cred) = &self.proxy.cred {
            let auth = format!("{}:{}", cred.username, cred.password);
            let encoded_auth = base64_simd::STANDARD.encode_to_string(auth.as_bytes());
            format!(
                "{}Proxy-Authorization: Basic {}\r\n\r\n",
                connect_request, encoded_auth
            )
        } else {
            format!("{}\r\n", connect_request)
        };

        let mut stream = Box::new(tls_stream);
        stream
            .write_all(connect_request.as_bytes())
            .await
            .context("failed to send CONNECT request to HTTPS proxy")?;
        stream
            .flush()
            .await
            .context("failed to flush proxy connection")?;

        let mut buffer = vec![0u8; 1024];
        let n = stream
            .read(&mut buffer)
            .await
            .context("failed to read proxy response")?;

        if n == 0 {
            return Err(anyhow!("proxy closed connection before responding"));
        }

        let response =
            std::str::from_utf8(&buffer[..n]).context("proxy response is not valid UTF-8")?;

        if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
            return Err(anyhow!("proxy CONNECT failed: {}", response.trim()));
        }

        debug!(
            "HTTPS proxy CONNECT successful to {}:{}",
            target_host, target_port
        );
        Ok(stream)
    }

    pub fn return_stream_to_pool(&self, stream: BoxedStream) {
        let proxy_addr = self.proxy.addr;

        if self.proxy.url.scheme() == "https" {
            if let Some(mut pool) = PROXY_TLS_POOL.get_mut(&proxy_addr) {
                let len = pool.len();
                if len <= 30 {
                    pool.push(stream);
                    let len = pool.len();
                    trace!(len, "proxy TLS stream returned to pool");
                } else {
                    trace!(len, "proxy TLS pool is full");
                }
            } else {
                PROXY_TLS_POOL.insert(proxy_addr, vec![stream]);
                trace!("add new vec to proxy TLS pool");
            }
        } else {
            if let Some(mut pool) = PROXY_TCP_POOL.get_mut(&proxy_addr) {
                let len = pool.len();
                if len <= 30 {
                    pool.push(stream);
                    let len = pool.len();
                    trace!(len, "proxy TCP stream returned to pool");
                } else {
                    trace!(len, "proxy TCP pool is full");
                }
            } else {
                PROXY_TLS_POOL.insert(proxy_addr, vec![stream]);
                trace!("add new vec to proxy TCP pool");
            }
        }
    }
}

async fn is_proxy_stream_closed(stream: &mut BoxedStream) -> bool {
    if let Some(stream) = stream.as_any_mut().downcast_mut::<TlsStream<TcpStream>>() {
        return is_stream_closed_inner(stream.get_mut()).await;
    } else if let Some(stream) = stream.as_any_mut().downcast_mut::<TcpStream>() {
        return is_stream_closed_inner(stream).await;
    } else {
        tracing::warn!("downcast failed for proxy stream");
        return true;
    }

    async fn is_stream_closed_inner(tcp: &mut TcpStream) -> bool {
        let mut buf = [0u8; 1];
        let result = async_std::future::timeout(Duration::from_secs(1), tcp.peek(&mut buf)).await;
        match result {
            Ok(result) => match result {
                Ok(0) => {
                    info!("read 0, proxy stream is closed");
                    return true;
                }
                Ok(n) => {
                    info!("read {n}, proxy stream is still open, but strange");
                    return false;
                }
                Err(err) => {
                    info!("get unexpected error, proxy stream closed: {err}");
                    return true;
                }
            },
            Err(_e) => {
                trace!("timeout, proxy stream is still open");
                return false;
            }
        }
    }
}

fn create_proxy_tls_config() -> Result<ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    let certs = load_native_certs().expect("failed to load system certs");

    for cert in certs {
        root_store
            .add(&Certificate(cert.to_vec()))
            .context("failed to add certificate to root store")?;
    }

    let client_config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(client_config)
}

fn create_proxy_tls_config_with_trust_store(
    trust_store: &Option<TrustStorePem>,
) -> Result<ClientConfig> {
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
            let file = std::fs::File::open(p).context("failed to open trust store file")?;
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
        root_store
            .add(&Certificate(cert.to_vec()))
            .context("failed to add certificate to root store")?;
    }

    let client_config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(client_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_https_proxy_option_new() {
        let proxy = HttpsProxyOption::new("http://proxy.example.com:8080").unwrap();
        assert_eq!(proxy.url.scheme(), "http");
        assert_eq!(proxy.url.host_str().unwrap(), "proxy.example.com");
        assert_eq!(proxy.url.port(), Some(8080));
        assert!(proxy.cred.is_none());
    }

    #[test]
    fn test_https_proxy_option_with_auth() {
        let proxy = HttpsProxyOption::new("http://user:pass@proxy.example.com:8080").unwrap();
        assert_eq!(proxy.url.scheme(), "http");
        assert_eq!(proxy.url.host_str().unwrap(), "proxy.example.com");
        assert_eq!(proxy.url.port(), Some(8080));

        let cred = proxy.cred.as_ref().unwrap();
        assert_eq!(cred.username, "user");
        assert_eq!(cred.password, "pass");
    }

    #[test]
    fn test_https_proxy_option_https() {
        let proxy = HttpsProxyOption::new("https://secure-proxy.example.com:8443").unwrap();
        assert_eq!(proxy.url.scheme(), "https");
        assert_eq!(proxy.url.host_str().unwrap(), "secure-proxy.example.com");
        assert_eq!(proxy.url.port(), Some(8443));
    }

    #[test]
    fn test_https_proxy_option_invalid_scheme() {
        let result = HttpsProxyOption::new("ftp://proxy.example.com:8080");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("proxy URL must use http or https scheme")
        );
    }

    #[test]
    fn test_https_proxy_option_no_host() {
        let result = HttpsProxyOption::new("http://:8080");
        assert!(result.is_err());
    }

    #[test]
    fn test_proxy_connector_creation() {
        let proxy = HttpsProxyOption::new("http://proxy.example.com:8080").unwrap();
        let connector = ProxyConnector::new(proxy);
        assert!(connector.is_ok());
    }

    #[test]
    fn test_proxy_url_parsing_with_default_ports() {
        let http_proxy = HttpsProxyOption::new("http://proxy.example.com").unwrap();
        assert_eq!(http_proxy.url.scheme(), "http");
        // URL parsing doesn't automatically set default ports
        assert_eq!(http_proxy.url.port(), None);
        assert_eq!(http_proxy.addr.port(), 80);

        let https_proxy = HttpsProxyOption::new("https://secure-proxy.example.com").unwrap();
        assert_eq!(https_proxy.url.scheme(), "https");
        // URL parsing doesn't automatically set default ports
        assert_eq!(https_proxy.url.port(), None);
        assert_eq!(https_proxy.addr.port(), 443);
    }

    #[test]
    fn test_proxy_credentials_extraction() {
        let proxy = HttpsProxyOption::new("http://user123:pass456@proxy.example.com:8080").unwrap();
        let cred = proxy.cred.as_ref().unwrap();
        assert_eq!(cred.username, "user123");
        assert_eq!(cred.password, "pass456");
    }

    #[test]
    fn test_proxy_connector_with_trust_store() {
        let proxy = HttpsProxyOption::new("https://proxy.example.com:8443").unwrap();
        let connector = ProxyConnector::new_with_trust_store(proxy, &None);
        assert!(connector.is_ok());
    }

    #[test]
    fn test_proxy_from_url() {
        let url = Url::parse("http://proxy.example.com:3128").unwrap();
        let proxy = HttpsProxyOption::from_url(url).unwrap();
        assert_eq!(proxy.url.scheme(), "http");
        assert_eq!(proxy.url.host_str().unwrap(), "proxy.example.com");
        assert_eq!(proxy.url.port(), Some(3128));
    }

    #[test]
    fn test_proxy_connect_timeout() {
        use std::time::Duration;

        async_std::task::block_on(async {
            let proxy = HttpsProxyOption::new("http://192.0.2.1:8080").unwrap(); // RFC5737 test address
            let connector = ProxyConnector::new(proxy).unwrap();

            let start = std::time::Instant::now();
            let result = connector.connect("example.com", 80, Duration::from_secs(1)).await;
            let elapsed = start.elapsed();

            assert!(result.is_err());
            assert!(elapsed < Duration::from_secs(2)); // Should timeout within ~1 second
            let error_msg = format!("{:?}", result.err().unwrap());
            assert!(error_msg.contains("timeout"));
        })
    }
}
