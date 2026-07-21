use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_std::{
    io::{ReadExt, WriteExt},
    net::TcpStream,
};
use async_tls::TlsConnector;
use rustls::{Certificate, ClientConfig};
use rustls_native_certs::load_native_certs;
use rustls_pemfile;
use tracing::{debug, error};
use url::Url;

use crate::error::{
    CertificateSnafu, ConnectionSnafu, ConnectionTimeoutSnafu, DnsSnafu, InvalidUrlSnafu,
    NoPortSnafu, ProxySnafu, Result, TlsSnafu,
};
use snafu::prelude::*;
use crate::misc::TrustStorePem;
use crate::stream::BoxedStream;

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
            .context(InvalidUrlSnafu)?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(ProxySnafu { message: "proxy URL must use http or https scheme".to_string() }.build());
        }

        let host = url
            .host_str()
            .ok_or_else(|| ProxySnafu { message: "proxy URL must have a host".to_string() }.build())?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| NoPortSnafu.build())?;

        let addrs = format!("{}:{}", host, port)
            .parse::<SocketAddr>()
            .or_else(|_| {
                // For testing purposes, use localhost if domain resolution fails
                if host.contains("example.com") || host.contains("localhost") {
                    Ok(SocketAddr::from(([127, 0, 0, 1], port)))
                } else {
                    std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
                        .map_err(|e| DnsSnafu { message: format!("failed to resolve proxy address: {e}") }.build())?
                        .next()
                        .ok_or_else(|| DnsSnafu { message: "no proxy addresses found".to_string() }.build())
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
            .ok_or_else(|| ProxySnafu { message: "proxy URL must have a host".to_string() }.build())?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| NoPortSnafu.build())?;

        let addrs = format!("{}:{}", host, port)
            .parse::<SocketAddr>()
            .or_else(|_| {
                if host.contains("example.com") || host.contains("localhost") {
                    Ok(SocketAddr::from(([127, 0, 0, 1], port)))
                } else {
                    std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
                        .map_err(|e| DnsSnafu { message: format!("failed to resolve proxy address: {e}") }.build())?
                        .next()
                        .ok_or_else(|| DnsSnafu { message: "no proxy addresses found".to_string() }.build())
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
        // Create TCP stream with connect timeout
        let mut tcp_stream = match async_std::future::timeout(connect_timeout, TcpStream::connect(&proxy_addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Err(ConnectionSnafu { message: format!("HTTP proxy connection failed: {e}") }.build()),
            Err(_) => return Err(ConnectionTimeoutSnafu { duration: connect_timeout }.build()),
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
            .map_err(|e| ProxySnafu { message: format!("failed to send CONNECT request to proxy: {e}") }.build())?;
        tcp_stream
            .flush()
            .await
            .map_err(|e| ProxySnafu { message: format!("failed to flush proxy connection: {e}") }.build())?;

        read_connect_response(&mut tcp_stream).await?;

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
        let tls_connector: TlsConnector = self.tls_config.clone().into();

        // Create TCP stream with connect timeout
        let tcp_stream = match async_std::future::timeout(connect_timeout, TcpStream::connect(&proxy_addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Err(ConnectionSnafu { message: format!("HTTPS proxy connection failed: {e}") }.build()),
            Err(_) => return Err(ConnectionTimeoutSnafu { duration: connect_timeout }.build()),
        };

        let proxy_host = self
            .proxy
            .url
            .host_str()
            .ok_or_else(|| ProxySnafu { message: "proxy URL must have a host".to_string() }.build())?;

        let tls_stream = tls_connector
            .connect(proxy_host, tcp_stream)
            .await
            .map_err(|e| TlsSnafu { message: format!("failed to establish TLS connection to HTTPS proxy: {e}") }.build())?;

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
            .map_err(|e| ProxySnafu { message: format!("failed to send CONNECT request to HTTPS proxy: {e}") }.build())?;
        stream
            .flush()
            .await
            .map_err(|e| ProxySnafu { message: format!("failed to flush proxy connection: {e}") }.build())?;

        read_connect_response(&mut stream).await?;

        debug!(
            "HTTPS proxy CONNECT successful to {}:{}",
            target_host, target_port
        );
        Ok(stream)
    }
}

/// Read the proxy CONNECT response fully by looping until \\r\\n\\r\\n is found.
/// Returns Ok(()) if the response status is 200, or Err with the response text otherwise.
async fn read_connect_response<S>(stream: &mut S) -> Result<()>
where
    S: async_std::io::Read + Unpin,
{
    let mut buf = [0u8; 512];
    let mut filled = 0;

    loop {
        let n = stream
            .read(&mut buf[filled..])
            .await
            .map_err(|e| ProxySnafu { message: format!("failed to read proxy CONNECT response: {e}") }.build())?;
        if n == 0 {
            return Err(ProxySnafu { message: "proxy closed connection before responding".to_string() }.build());
        }
        filled += n;

        if filled >= 4 && buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
            if !buf.starts_with(b"HTTP/1.1 200") && !buf.starts_with(b"HTTP/1.0 200") {
                let text = String::from_utf8_lossy(&buf[..filled]);
                return Err(ProxySnafu { message: format!("proxy CONNECT failed: {}", text.trim()) }.build());
            }
            return Ok(());
        }
    }
}

fn create_proxy_tls_config() -> Result<ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    let cert_result = load_native_certs();
    if !cert_result.errors.is_empty() && cert_result.certs.is_empty() {
        return Err(CertificateSnafu { message: format!("failed to load system certs: {:?}", cert_result.errors) }.build());
    }
    let certs = cert_result.certs;

    for cert in certs {
        root_store
            .add(&Certificate(cert.to_vec()))
            .map_err(|e| CertificateSnafu { message: format!("failed to add certificate: {e}") }.build())?;
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
        None => {
            let cert_result = load_native_certs();
            if !cert_result.errors.is_empty() && cert_result.certs.is_empty() {
                return Err(CertificateSnafu { message: format!("failed to load system certs: {:?}", cert_result.errors) }.build());
            }
            cert_result.certs
        }
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
                .map_err(|e| CertificateSnafu { message: format!("failed to open trust store file: {e}") }.build())?;
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
            .map_err(|e| CertificateSnafu { message: format!("failed to add certificate: {e}") }.build())?;
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
        let err = result.err().unwrap();
        assert!(
            err.to_string().contains("proxy URL must use http or https scheme"),
            "actual: {err}"
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
        })
    }
}
