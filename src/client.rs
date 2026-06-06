use anyhow_ext::{Context, Result, anyhow};
use async_std::{
    future::{self, timeout},
    io::{ReadExt, WriteExt},
    net::TcpStream,
};
use rand::seq::IndexedRandom;

use async_tls::TlsConnector;
use dashmap::DashMap;
use derive_builder::Builder;
use nom::{
    IResult, Parser,
    bytes::complete::{is_not, tag, take_till},
};

use rustls_native_certs::load_native_certs;
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    body::Body,
    misc::TrustStorePem,
    proxy::{HttpsProxyOption, ProxyConnector},
    requestx::Request,
    response::Response,
    stream::BoxedStream,
};
use tracing::{error, trace};

/// Connection type for pool key
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum ConnectionType {
    /// Direct TCP connection (no proxy)
    DirectTcp,
    /// Direct TLS connection (no proxy)
    DirectTls,
    /// Connection through HTTP proxy
    ProxyTcp(SocketAddr),
    /// Connection through HTTPS proxy
    ProxyTls(SocketAddr),
}

/// Key for identifying connections in the pool
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct ConnectionKey {
    /// The remote server address
    pub(crate) addr: SocketAddr,
    /// Type of connection
    pub(crate) connection_type: ConnectionType,
}

/// A pooled connection with metadata for idle eviction.
pub(crate) struct PooledConnection {
    pub stream: BoxedStream,
    pub returned_at: Instant,
}

/// Thread-safe connection pool with per-key and global limits plus idle eviction.
pub(crate) struct ConnectionPoolInner {
    map: DashMap<ConnectionKey, Vec<PooledConnection>>,
    total_count: AtomicUsize,
    max_per_key: usize,
    max_total: usize,
    idle_timeout: Duration,
}

impl ConnectionPoolInner {
    pub fn new(max_per_key: usize, max_total: usize, idle_timeout: Duration) -> Self {
        Self {
            map: DashMap::new(),
            total_count: AtomicUsize::new(0),
            max_per_key,
            max_total,
            idle_timeout,
        }
    }

    /// Pick a non-idle connection for the given key. Discards expired connections
    /// and removes empty entries. Returns None if no usable connection exists.
    pub fn pick(&self, key: &ConnectionKey) -> Option<BoxedStream> {
        let mut entry = match self.map.get_mut(key) {
            Some(e) => e,
            None => return None,
        };
        let pool = entry.value_mut();
        while let Some(conn) = pool.pop() {
            if conn.returned_at.elapsed() < self.idle_timeout {
                self.total_count.fetch_sub(1, Ordering::Relaxed);
                let is_empty = pool.is_empty();
                drop(entry);
                if is_empty {
                    self.map.remove(key);
                }
                return Some(conn.stream);
            }
            self.total_count.fetch_sub(1, Ordering::Relaxed);
            trace!(key = ?(&key.addr, &key.connection_type), "discarded idle connection");
        }
        drop(entry);
        self.map.remove(key);
        None
    }

    /// Return a stream to the pool. Enforces both per-key and global limits.
    /// Cleans up idle connections for this key as a side effect.
    pub fn return_stream(&self, stream: BoxedStream, stream_info: StreamInfo) {
        let key = build_connection_key(&stream_info);

        // Evict idle connections for this key
        self.evict_idle_for_key(&key);

        // Check global limit
        if self.total_count.load(Ordering::Relaxed) >= self.max_total {
            trace!(key = ?(&key.addr, &key.connection_type), "global pool full, dropping stream");
            return;
        }

        use dashmap::mapref::entry::Entry;
        match self.map.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                let pool = entry.get_mut();
                if pool.len() < self.max_per_key {
                    pool.push(PooledConnection {
                        stream,
                        returned_at: Instant::now(),
                    });
                    self.total_count.fetch_add(1, Ordering::Relaxed);
                    trace!(key = ?(&key.addr, &key.connection_type), len = pool.len(), "stream returned to pool");
                } else {
                    trace!(key = ?(&key.addr, &key.connection_type), len = pool.len(), "per-key pool full");
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(vec![PooledConnection {
                    stream,
                    returned_at: Instant::now(),
                }]);
                self.total_count.fetch_add(1, Ordering::Relaxed);
                trace!(key = ?(&key.addr, &key.connection_type), "add new vec to pool");
            }
        }
    }

    /// Remove expired connections for a given key and adjust total_count.
    fn evict_idle_for_key(&self, key: &ConnectionKey) {
        if let Some(mut entry) = self.map.get_mut(key) {
            let pool = entry.value_mut();
            let before = pool.len();
            pool.retain(|conn| conn.returned_at.elapsed() < self.idle_timeout);
            let evicted = before - pool.len();
            if evicted > 0 {
                self.total_count.fetch_sub(evicted, Ordering::Relaxed);
                trace!(key = ?(&key.addr, &key.connection_type), evicted, "evicted idle connections");
            }
        }
    }
}

/// Build a ConnectionKey from StreamInfo.
fn build_connection_key(stream_info: &StreamInfo) -> ConnectionKey {
    if let Some(proxy) = &stream_info.proxy_used {
        match proxy.url.scheme() {
            "https" => ConnectionKey {
                addr: proxy.addr,
                connection_type: ConnectionType::ProxyTls(proxy.addr),
            },
            _ => ConnectionKey {
                addr: proxy.addr,
                connection_type: ConnectionType::ProxyTcp(proxy.addr),
            },
        }
    } else if stream_info.is_tls {
        ConnectionKey {
            addr: stream_info.addr,
            connection_type: ConnectionType::DirectTls,
        }
    } else {
        ConnectionKey {
            addr: stream_info.addr,
            connection_type: ConnectionType::DirectTcp,
        }
    }
}

pub(crate) type ConnectionPool = Arc<ConnectionPoolInner>;

/// Connection metadata for returning streams to the appropriate pool
#[derive(Clone)]
pub(crate) struct StreamInfo {
    /// The socket address of the remote server
    pub addr: SocketAddr,
    /// Whether this is a TLS connection
    pub is_tls: bool,
    /// Proxy configuration that was used for this connection
    pub proxy_used: Option<HttpsProxyOption>,
}

/// HTTP client with configurable timeouts and proxy settings
#[derive(Builder, Clone)]
#[builder(setter(strip_option, prefix = "set"))]
pub struct ZJHttpClient {
    #[builder(default = "Duration::from_secs(30)")]
    pub global_send_header_timeout: Duration,
    #[builder(default = "Duration::from_secs(30)")]
    pub global_read_header_timeout: Duration,
    #[builder(default)]
    pub global_read_body_timeout: Option<Duration>,
    #[builder(default = "Duration::from_secs(3)")]
    pub global_connect_timeout: Duration,
    #[builder(default)]
    pub global_trust_store_pem: Option<TrustStorePem>,
    #[builder(default)]
    pub global_proxy: Option<HttpsProxyOption>,
    #[builder(default = "64 * 1024")]
    pub global_max_header_bytes: usize,
    #[builder(default = "Arc::new(ConnectionPoolInner::new(30, 1000, Duration::from_secs(90)))")]
    pub(crate) connection_pool: ConnectionPool,
}

impl std::fmt::Debug for ZJHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZJHttpClient")
            .field("global_send_header_timeout", &self.global_send_header_timeout)
            .field("global_read_header_timeout", &self.global_read_header_timeout)
            .field("global_read_body_timeout", &self.global_read_body_timeout)
            .field("global_connect_timeout", &self.global_connect_timeout)
            .field("global_trust_store_pem", &self.global_trust_store_pem)
            .field("global_proxy", &self.global_proxy)
            .field("global_max_header_bytes", &self.global_max_header_bytes)
            .field("connection_pool", &format!("<pool with {} entries, {} connections>",
                self.connection_pool.map.len(),
                self.connection_pool.total_count.load(Ordering::Relaxed)))
            .finish()
    }
}

impl ZJHttpClient {
    /// Create a builder for ZJHttpClient with default values
    pub fn builder() -> ZJHttpClientBuilder {
        ZJHttpClientBuilder {
            global_send_header_timeout: Some(Duration::from_secs(30)),
            global_read_header_timeout: Some(Duration::from_secs(30)),
            global_read_body_timeout: None,
            global_connect_timeout: Some(Duration::from_secs(3)),
            global_trust_store_pem: None,
            global_proxy: None,
            global_max_header_bytes: Some(64 * 1024),
            connection_pool: Some(Arc::new(ConnectionPoolInner::new(30, 1000, Duration::from_secs(90)))),
        }
    }

    pub fn set_proxy(mut self, proxy: HttpsProxyOption) -> Self {
        self.global_proxy = Some(proxy);
        self
    }

    pub fn set_proxy_from_url(mut self, proxy_url: impl AsRef<str>) -> Result<Self> {
        let proxy = HttpsProxyOption::new(proxy_url)?;
        self.global_proxy = Some(proxy);
        Ok(self)
    }

    pub fn set_connect_timeout(mut self, timeout: Duration) -> Self {
        self.global_connect_timeout = timeout;
        self
    }

    pub fn set_pool_config(mut self, max_per_key: usize, max_total: usize, idle_timeout: Duration) -> Self {
        self.connection_pool = Arc::new(ConnectionPoolInner::new(max_per_key, max_total, idle_timeout));
        self
    }

    pub async fn send(&self, req: &mut Request) -> Result<Response> {
        let addr = resolve_1st_ip(req).await.dot()?;
        let (mut stream, reused) = pick_or_connect_stream(self, &req, &addr).await.dot()?;

        // If send_header fails on a reused (pooled) connection, it's likely stale.
        // Retry once with a fresh connection — body hasn't been consumed yet, so retry is safe.
        if let Err(e) = send_header(self, req, &mut stream).await {
            if reused {
                trace!(
                    "pooled connection failed during send_header, retrying with fresh connection"
                );
                drop(stream);
                stream = connect_fresh_stream(self, &req, &addr).await.dot()?;
                send_header(self, req, &mut stream).await.dot()?;
            } else {
                return Err(e);
            }
        }

        send_body(req, &mut stream).await.dot()?;
        match read_headers_to_resp(self, req, stream, addr).await {
            Ok(resp) => Ok(resp),
            Err(e) if reused && !matches!(req.body, Body::Stream(_)) => {
                trace!(
                    "pooled connection failed during read_headers_to_resp, retrying with fresh connection: {e:#}"
                );
                let mut stream =
                    connect_fresh_stream(self, &req, &addr).await.dot()?;
                send_header(self, req, &mut stream).await.dot()?;
                send_body(req, &mut stream).await.dot()?;
                read_headers_to_resp(self, req, stream, addr).await
            }
            Err(e) => Err(e),
        }
    }

    pub async fn send_header_only(&self, req: &mut Request) -> Result<(BoxedStream, SocketAddr)> {
        let addr = resolve_1st_ip(req).await.dot()?;
        let (mut stream, reused) = pick_or_connect_stream(self, &req, &addr).await.dot()?;

        if let Err(e) = send_header(self, req, &mut stream).await {
            if reused {
                trace!(
                    "pooled connection failed during send_header, retrying with fresh connection"
                );
                drop(stream);
                stream = connect_fresh_stream(self, &req, &addr).await.dot()?;
                send_header(self, req, &mut stream).await.dot()?;
            } else {
                return Err(e);
            }
        }

        Ok((stream, addr))
    }

    pub async fn send_body_only(
        &self,
        req: &mut Request,
        mut stream_to_write: BoxedStream,
        addr: SocketAddr,
    ) -> Result<Response> {
        send_body(req, &mut stream_to_write).await.dot()?;
        let resp = read_headers_to_resp(self, req, stream_to_write, addr)
            .await
            .dot()?;
        return Ok(resp);
    }
}

/// Try to pick a stream from the connection pool, or create a new one.
/// Returns (stream, true) if reused from pool, (stream, false) if freshly created.
async fn pick_or_connect_stream(
    client: &ZJHttpClient,
    req: &Request,
    addr: &SocketAddr,
) -> Result<(BoxedStream, bool)> {
    // Determine which proxy to use (request-level takes precedence over client-level)
    let proxy = req.proxy.as_ref().or(client.global_proxy.as_ref());

    if let Some(proxy_option) = proxy {
        let connection_type = if proxy_option.url.scheme() == "https" {
            ConnectionType::ProxyTls(proxy_option.addr)
        } else {
            ConnectionType::ProxyTcp(proxy_option.addr)
        };

        let key = ConnectionKey {
            addr: *addr,
            connection_type,
        };

        if let Some(stream_from_pool) = try_pick_from_pool(&client.connection_pool, &key) {
            trace!(?addr, "picking up proxy stream from pool");
            return Ok((stream_from_pool, true));
        }

        let proxy_connector = if let Some(trust_store) = &req.trust_store_pem {
            ProxyConnector::new_with_trust_store(proxy_option.clone(), &Some(trust_store.clone()))?
        } else {
            ProxyConnector::new_with_trust_store(
                proxy_option.clone(),
                &client.global_trust_store_pem,
            )?
        };

        let target_host = req.url.host_str().ok_or(anyhow!("no host in URL"))?;
        let target_port = req
            .url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("URL must have a valid port"))?;

        let connect_timeout = req.connect_timeout.unwrap_or(client.global_connect_timeout);
        let stream = proxy_connector
            .connect(target_host, target_port, connect_timeout)
            .await?;
        return Ok((stream, false));
    }

    match req.url.scheme() {
        "http" => {
            let key = ConnectionKey {
                addr: *addr,
                connection_type: ConnectionType::DirectTcp,
            };

            if let Some(stream_from_pool) = try_pick_from_pool(&client.connection_pool, &key) {
                trace!(?addr, "picking up direct TCP stream from pool");
                return Ok((stream_from_pool, true));
            }
            trace!(?addr, "no existing TCP connection for this addr");
            let stream = connect_fresh_tcp(client, req, addr).await?;
            Ok((stream, false))
        }
        "https" => {
            let key = ConnectionKey {
                addr: *addr,
                connection_type: ConnectionType::DirectTls,
            };

            if let Some(stream_from_pool) = try_pick_from_pool(&client.connection_pool, &key) {
                trace!(?addr, "picking up direct TLS stream from pool");
                return Ok((stream_from_pool, true));
            }
            trace!(?addr, "no existing TLS connection for this addr");
            let stream = connect_fresh_tls(client, req, addr).await?;
            Ok((stream, false))
        }
        others => Err(anyhow!("scheme {others} is not supported at the moment")),
    }
}

/// Create a fresh connection, skipping the pool entirely.
/// Used for retry after a stale pooled connection fails.
async fn connect_fresh_stream(
    client: &ZJHttpClient,
    req: &Request,
    addr: &SocketAddr,
) -> Result<BoxedStream> {
    match req.url.scheme() {
        "http" => connect_fresh_tcp(client, req, addr).await,
        "https" => connect_fresh_tls(client, req, addr).await,
        others => Err(anyhow!("scheme {others} is not supported at the moment")),
    }
}

async fn connect_fresh_tcp(
    client: &ZJHttpClient,
    req: &Request,
    addr: &SocketAddr,
) -> Result<BoxedStream> {
    let connect_timeout = req.connect_timeout.unwrap_or(client.global_connect_timeout);
    let tcp_stream = match timeout(connect_timeout, TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return Err(anyhow!("TCP connection failed: {e}")),
        Err(_) => {
            return Err(anyhow!(
                "TCP connection timeout after {:?}",
                connect_timeout
            ));
        }
    };
    Ok(Box::new(tcp_stream))
}

async fn connect_fresh_tls(
    client: &ZJHttpClient,
    req: &Request,
    addr: &SocketAddr,
) -> Result<BoxedStream> {
    let connect_timeout = req.connect_timeout.unwrap_or(client.global_connect_timeout);
    let tls_config = create_tls_config(&client.global_trust_store_pem).dot()?;
    let tls_connector: TlsConnector = Arc::new(tls_config).into();
    let host = match req.url.host() {
        Some(url::Host::Domain(s)) => s,
        _ => {
            return Err(anyhow!(
                "HTTPS request should specify the Domain instead of IP, or you can provide the sni domain name"
            ));
        }
    };
    let tcp_stream = match timeout(connect_timeout, TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return Err(anyhow!("TCP connection failed: {e}")),
        Err(_) => {
            return Err(anyhow!(
                "TCP connection timeout after {:?}",
                connect_timeout
            ));
        }
    };
    let tls_stream = tls_connector.connect(host, tcp_stream).await.dot()?;
    Ok(Box::new(tls_stream))
}

fn try_pick_from_pool(pool: &ConnectionPool, key: &ConnectionKey) -> Option<BoxedStream> {
    pool.pick(key)
}

async fn resolve_1st_ip(req: &mut Request) -> Result<SocketAddr> {
    let addrs = req.url.socket_addrs(|| None).dot()?;
    if addrs.is_empty() {
        return Err(anyhow!("no result in DNS resolve"));
    }
    let mut rng = rand::rng();
    let addr = addrs
        .choose(&mut rng)
        .ok_or(anyhow!("no result in DNS resolve"))?
        .to_owned();
    Ok(addr)
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

async fn send_header<S>(client: &ZJHttpClient, req: &Request, stream: &mut S) -> Result<()>
where
    S: async_std::io::Read + async_std::io::Write + Unpin + Send + Sync + 'static,
{
    // Apply send header timeout
    let timeout_dur = req
        .send_header_timeout
        .unwrap_or(client.global_send_header_timeout);
    let send_future = async {
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
        // Write Content-Type if set and user hasn't manually set it in headers
        if let Some(ct) = req.content_type {
            let already_set = req
                .headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("content-type"));
            if !already_set {
                stream.write_all(b"Content-Type: ").await.dot()?;
                stream.write_all(ct.as_bytes()).await.dot()?;
                stream.write_all(b"\r\n").await.dot()?;
            }
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
    };

    match future::timeout(timeout_dur, send_future).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("send header timeout after {:?}", timeout_dur)),
    }
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
        Body::Bytes(bytes) => {
            stream_to_write.write_all(&bytes).await.dot()?;
        }
        Body::MultipartForm(form) => {
            // Serialize multipart form data
            let boundary = form.boundary().to_string();
            let boundary_bytes = boundary.as_bytes();

            // Take ownership of fields to consume them
            let fields = std::mem::take(&mut form.fields);

            for field in fields {
                // Write boundary
                stream_to_write.write_all(b"--").await.dot()?;
                stream_to_write.write_all(boundary_bytes).await.dot()?;
                stream_to_write.write_all(b"\r\n").await.dot()?;

                match field {
                    crate::body::MultipartField::Text(name, value) => {
                        stream_to_write
                            .write_all(
                                format!(
                                    "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                                    name
                                )
                                .as_bytes(),
                            )
                            .await
                            .dot()?;
                        stream_to_write.write_all(value.as_bytes()).await.dot()?;
                        stream_to_write.write_all(b"\r\n").await.dot()?;
                    }
                    crate::body::MultipartField::FilePath(
                        name,
                        path,
                        filename_opt,
                        content_type_opt,
                    ) => {
                        let filename =
                            filename_opt
                                .as_ref()
                                .map(|f| f.as_str())
                                .unwrap_or_else(|| {
                                    path.file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("filename")
                                });
                        let content_type = content_type_opt
                            .as_ref()
                            .map(|c| c.as_str())
                            .unwrap_or_else(|| crate::body::detect_mime_type(filename));

                        stream_to_write
                            .write_all(format!(
                                "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                                name, filename
                            ).as_bytes())
                            .await.dot()?;
                        stream_to_write
                            .write_all(format!("Content-Type: {}\r\n\r\n", content_type).as_bytes())
                            .await
                            .dot()?;

                        // Read and write file content
                        let mut file = async_std::fs::File::open(path).await.dot()?;
                        let mut buf = vec![0u8; 1024 * 64]; // 64KB buffer
                        loop {
                            let n = file.read(&mut buf).await.dot()?;
                            if n == 0 {
                                break;
                            }
                            stream_to_write.write_all(&buf[..n]).await.dot()?;
                        }
                        stream_to_write.write_all(b"\r\n").await.dot()?;
                    }
                    crate::body::MultipartField::File(
                        name,
                        file,
                        filename_opt,
                        content_type_opt,
                    ) => {
                        let filename = filename_opt
                            .as_ref()
                            .map(|f| f.as_str())
                            .unwrap_or("filename");
                        let content_type = content_type_opt
                            .as_ref()
                            .map(|c| c.as_str())
                            .unwrap_or_else(|| crate::body::detect_mime_type(filename));

                        stream_to_write
                            .write_all(format!(
                                "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                                name, filename
                            ).as_bytes())
                            .await.dot()?;
                        stream_to_write
                            .write_all(format!("Content-Type: {}\r\n\r\n", content_type).as_bytes())
                            .await
                            .dot()?;

                        // Read and write file content
                        let mut file = file;
                        let mut buf = vec![0u8; 1024 * 64]; // 64KB buffer
                        loop {
                            let n = file.read(&mut buf).await.dot()?;
                            if n == 0 {
                                break;
                            }
                            stream_to_write.write_all(&buf[..n]).await.dot()?;
                        }
                        stream_to_write.write_all(b"\r\n").await.dot()?;
                    }
                    crate::body::MultipartField::Stream(
                        name,
                        mut stream,
                        filename_opt,
                        content_type_opt,
                    ) => {
                        let filename = filename_opt
                            .as_ref()
                            .map(|f| f.as_str())
                            .unwrap_or("filename");
                        let content_type = content_type_opt
                            .as_ref()
                            .map(|c| c.as_str())
                            .unwrap_or_else(|| crate::body::detect_mime_type(filename));

                        stream_to_write
                            .write_all(format!(
                                "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                                name, filename
                            ).as_bytes())
                            .await.dot()?;
                        stream_to_write
                            .write_all(format!("Content-Type: {}\r\n\r\n", content_type).as_bytes())
                            .await
                            .dot()?;

                        // Read and write stream content
                        let mut buf = vec![0u8; 1024 * 64]; // 64KB buffer
                        loop {
                            let n = stream.read(&mut buf).await.dot()?;
                            if n == 0 {
                                break;
                            }
                            stream_to_write.write_all(&buf[..n]).await.dot()?;
                        }
                        stream_to_write.write_all(b"\r\n").await.dot()?;
                    }
                }
            }

            // Write final boundary
            stream_to_write.write_all(b"--").await.dot()?;
            stream_to_write.write_all(boundary_bytes).await.dot()?;
            stream_to_write.write_all(b"--\r\n").await.dot()?;
        }
    }
    Ok(())
}

async fn read_headers_to_resp(
    client: &ZJHttpClient,
    req: &mut Request,
    mut stream: BoxedStream,
    addr: SocketAddr,
) -> Result<Response> {
    // Determine which proxy was used (request-level takes precedence over client-level)
    let proxy_used = req.proxy.as_ref().or(client.global_proxy.as_ref()).cloned();

    // Read all headers at once (including status line) until \r\n\r\n
    let (all_headers, overflow, overflow_len) = {
        let fut = read_until(&mut stream, b"\r\n\r\n", client.global_max_header_bytes);
        let dur = req
            .read_header_timeout
            .unwrap_or(client.global_read_header_timeout);
        future::timeout(dur, fut).await.dot()??
    };

    let input = std::str::from_utf8(&all_headers).dot()?;

    // Parse the first line (status line)
    let (remaining, (_, http_version, _, status_code, _)) = parse_resp_first_line(input)
        .map_err(|e| {
            anyhow!(
                "{err}:parse resp first line failed. data={input}",
                err = e.to_owned(),
            )
        })
        .dot()?;

    // Parse the remaining headers
    let headers = parse_headers(remaining)
        .dot()?
        .into_iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), value.to_owned()))
        .collect::<Vec<_>>();

    // Determine read body timeout (request-level takes precedence over client-level)
    let read_body_timeout = req.read_body_timeout.or(client.global_read_body_timeout);

    return Response::new_from_parse_result(
        http_version,
        status_code,
        headers,
        stream,
        req.url.scheme() == "https",
        addr,
        proxy_used,
        read_body_timeout,
        &overflow[..overflow_len],
        Some(client.connection_pool.clone()),
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
    (
        is_not(": "),
        tag(": "),
        take_till(|x| x == '\r' || x == '\n'),
        tag("\r\n"),
    )
        .parse(input)
}

fn parse_resp_first_line(input: &str) -> IResult<&str, (&str, &str, &str, &str, &str)> {
    (
        tag("HTTP/"),
        take_till(|x| x == ' '),
        tag(" "),
        take_till(|x| x == ' ' || x == '\r'), // status message is not mandortory
        take_till(|x| x == '\n'),
    )
        .parse(input)
}

// TODO: use nom to parse stream
/// Read from stream until delimiter is found. Returns (data, overflow).
/// Data includes everything up to and including the delimiter.
/// Overflow contains any bytes read past the delimiter.
pub async fn read_until<S>(
    stream: &mut S,
    delimiter: &[u8],
    max_bytes: usize,
) -> Result<(Vec<u8>, [u8; 4096], usize)>
where
    S: async_std::io::Read + Unpin + Send + Sync + 'static,
{
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    if delimiter.is_empty() {
        return Ok((buf, [0u8; 4096], 0));
    }

    loop {
        let n = stream.read(&mut tmp).await.dot()?;
        if n == 0 {
            return Err(anyhow!(
                "unexpected EOF while reading until delimiter (read {} bytes)",
                buf.len()
            ));
        }

        buf.extend_from_slice(&tmp[..n]);

        if buf.len() > max_bytes {
            return Err(anyhow!(
                "read_until exceeded max_bytes limit ({} > {})",
                buf.len(),
                max_bytes
            ));
        }

        // Search the tail that could contain a straddling delimiter
        let check_start = buf.len().saturating_sub(n + delimiter.len() - 1);
        if let Some(pos) = buf[check_start..]
            .windows(delimiter.len())
            .position(|w| w == delimiter)
        {
            let end = check_start + pos + delimiter.len();
            let overflow_len = buf.len() - end;
            let mut overflow = [0u8; 4096];
            overflow[..overflow_len].copy_from_slice(&buf[end..]);
            buf.truncate(end);
            return Ok((buf, overflow, overflow_len));
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
    use async_std::io::Cursor;

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

    #[test]
    fn test_client_proxy_configuration() {
        let mut client = ZJHttpClient::builder().build().unwrap();
        assert!(client.global_proxy.is_none());

        let proxy = HttpsProxyOption::new("http://proxy.example.com:8080").unwrap();
        client = client.set_proxy(proxy.clone());
        assert!(client.global_proxy.is_some());
        assert_eq!(
            client.global_proxy.unwrap().url.host_str().unwrap(),
            "proxy.example.com"
        );
    }

    #[test]
    fn test_client_proxy_from_url() {
        let result = ZJHttpClient::builder()
            .build()
            .unwrap()
            .set_proxy_from_url("http://proxy.example.com:8080");
        assert!(result.is_ok());
        let client = result.unwrap();
        assert!(client.global_proxy.is_some());
        assert_eq!(
            client.global_proxy.unwrap().url.host_str().unwrap(),
            "proxy.example.com"
        );
    }

    #[test]
    fn test_client_invalid_proxy_url() {
        let result = ZJHttpClient::builder()
            .build()
            .unwrap()
            .set_proxy_from_url("invalid-url");
        assert!(result.is_err());
    }

    #[test]
    fn test_client_connect_timeout_default() {
        let client = ZJHttpClient::builder().build().unwrap();
        assert_eq!(client.global_connect_timeout, Duration::from_secs(3));
    }

    #[test]
    fn test_client_connect_timeout_custom() {
        let client = ZJHttpClient::builder()
            .set_global_connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        assert_eq!(client.global_connect_timeout, Duration::from_secs(10));
    }

    // ==================== read_until tests ====================

    #[async_std::test]
    async fn test_read_until_basic() {
        let data = b"Hello World\r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, overflow, overflow_len) = result.unwrap();
        assert_eq!(buf, b"Hello World\r\n");
        assert_eq!(&overflow[..overflow_len], b"");
    }

    #[async_std::test]
    async fn test_read_until_single_char_delimiter() {
        let data = b"Hello\nWorld";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, overflow, overflow_len) = result.unwrap();
        assert_eq!(buf, b"Hello\n");
        assert_eq!(&overflow[..overflow_len], b"World");
    }

    #[async_std::test]
    async fn test_read_until_empty_delimiter() {
        let data = b"Hello World";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, overflow, overflow_len) = result.unwrap();
        assert_eq!(buf, b"");
        assert_eq!(&overflow[..overflow_len], b"");
    }

    #[async_std::test]
    async fn test_read_until_no_delimiter_found() {
        let data = b"Hello World";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n", 1024 * 1024).await;
        assert!(result.is_err());
    }

    #[async_std::test]
    async fn test_read_until_delimiter_at_start() {
        let data = b"\r\nHello World";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, overflow, overflow_len) = result.unwrap();
        assert_eq!(buf, b"\r\n");
        assert_eq!(&overflow[..overflow_len], b"Hello World");
    }

    #[async_std::test]
    async fn test_read_until_empty_stream() {
        let data = b"";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n", 1024 * 1024).await;
        assert!(result.is_err());
    }

    #[async_std::test]
    async fn test_read_until_multiple_delimiters() {
        let data = b"Line1\r\nLine2\r\nLine3\r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, overflow, overflow_len) = result.unwrap();
        assert_eq!(buf, b"Line1\r\n");
        assert_eq!(&overflow[..overflow_len], b"Line2\r\nLine3\r\n");
    }

    #[async_std::test]
    async fn test_read_until_long_delimiter() {
        let data = b"Some data\r\n\r\nMore data";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, overflow, overflow_len) = result.unwrap();
        assert_eq!(buf, b"Some data\r\n\r\n");
        assert_eq!(&overflow[..overflow_len], b"More data");
    }

    // ==================== HTTP header tests ====================

    #[async_std::test]
    async fn test_read_until_http_response_first_line() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        assert_eq!(buf, b"HTTP/1.1 200 OK\r\n");
        let text = std::str::from_utf8(&buf).unwrap();
        assert_eq!(text, "HTTP/1.1 200 OK\r\n");
    }

    #[async_std::test]
    async fn test_read_until_http_headers_complete() {
        // This is the key test - reading complete HTTP headers until \r\n\r\n
        let data = b"HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: 1234\r\n\
                     Connection: keep-alive\r\n\
                     \r\n\
                     {\"message\": \"body\"}";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        let text = std::str::from_utf8(&buf).unwrap();

        // Verify we got all headers but not the body
        assert!(text.contains("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains("Content-Length: 1234\r\n"));
        assert!(text.contains("Connection: keep-alive\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
        assert!(!text.contains("{\"message\": \"body\"}"));
    }

    #[async_std::test]
    async fn test_read_until_http_request_headers() {
        let data = b"GET /index.html HTTP/1.1\r\n\
                     Host: www.example.com\r\n\
                     User-Agent: Mozilla/5.0\r\n\
                     Accept: */*\r\n\
                     \r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        let text = std::str::from_utf8(&buf).unwrap();

        assert!(text.contains("GET /index.html HTTP/1.1\r\n"));
        assert!(text.contains("Host: www.example.com\r\n"));
        assert!(text.contains("User-Agent: Mozilla/5.0\r\n"));
        assert!(text.contains("Accept: */*\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[async_std::test]
    async fn test_read_until_http_headers_with_special_characters() {
        let data = b"HTTP/1.1 200 OK\r\n\
                     Content-Type: text/html; charset=utf-8\r\n\
                     Set-Cookie: session=abc123; Path=/; HttpOnly\r\n\
                     Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9\r\n\
                     \r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        let text = std::str::from_utf8(&buf).unwrap();

        assert!(text.contains("Content-Type: text/html; charset=utf-8\r\n"));
        assert!(text.contains("Set-Cookie: session=abc123; Path=/; HttpOnly\r\n"));
        assert!(text.contains("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[async_std::test]
    async fn test_read_until_http_headers_multiline_value() {
        // Test with folded header values (deprecated but still valid in some cases)
        let data = b"HTTP/1.1 200 OK\r\n\
                     Content-Type: text/html\r\n\
                     X-Custom: line1\r\n\
                      line2\r\n\
                     \r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        let text = std::str::from_utf8(&buf).unwrap();

        assert!(text.contains("HTTP/1.1 200 OK\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[async_std::test]
    async fn test_read_until_http_headers_many_headers() {
        // Test with many headers to ensure buffer can handle it
        let mut data = String::from("HTTP/1.1 200 OK\r\n");
        for i in 0..50 {
            data.push_str(&format!("X-Header-{}: value{}\r\n", i, i));
        }
        data.push_str("\r\n");

        let data_bytes = data.into_bytes();
        let mut cursor = Cursor::new(data_bytes);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        let text = std::str::from_utf8(&buf).unwrap();

        assert!(text.contains("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("X-Header-0: value0\r\n"));
        assert!(text.contains("X-Header-49: value49\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[async_std::test]
    async fn test_read_until_http_headers_empty_values() {
        let data = b"HTTP/1.1 200 OK\r\n\
                     X-Empty-1: \r\n\
                     X-Empty-2: \r\n\
                     \r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        let text = std::str::from_utf8(&buf).unwrap();

        assert!(text.contains("X-Empty-1: \r\n"));
        assert!(text.contains("X-Empty-2: \r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[async_std::test]
    async fn test_read_until_http_response_with_chunked_encoding() {
        let data = b"HTTP/1.1 200 OK\r\n\
                     Transfer-Encoding: chunked\r\n\
                     Content-Type: text/plain\r\n\
                     \r\n\
                     5\r\n\
                     Hello\r\n\
                     0\r\n\
                     \r\n";
        let mut cursor = Cursor::new(data);
        let result = read_until(&mut cursor, b"\r\n\r\n", 1024 * 1024).await;
        assert!(result.is_ok());
        let (buf, _, _) = result.unwrap();
        let text = std::str::from_utf8(&buf).unwrap();

        assert!(text.contains("Transfer-Encoding: chunked\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
        // Should not include the chunked body
        assert!(!text.contains("5\r\n"));
    }

    // ==================== Connection pool tests ====================

    struct MockStream {
        data: Vec<u8>,
        pos: usize,
    }
    impl MockStream {
        fn new(data: &[u8]) -> Self {
            Self { data: data.to_vec(), pos: 0 }
        }
    }
    impl async_std::io::Read for MockStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let n = std::cmp::min(buf.len(), self.data.len() - self.pos);
            if n == 0 { return std::task::Poll::Ready(Ok(0)); }
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            std::task::Poll::Ready(Ok(n))
        }
    }
    impl async_std::io::Write for MockStream {
        fn poll_write(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>, _buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Ok(0))
        }
        fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> { std::task::Poll::Ready(Ok(())) }
        fn poll_close(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> { std::task::Poll::Ready(Ok(())) }
    }
    impl crate::stream::RWStream for MockStream {}

    fn make_stream() -> BoxedStream {
        Box::new(MockStream::new(b"test"))
    }

    fn make_key() -> ConnectionKey {
        ConnectionKey {
            addr: "127.0.0.1:8080".parse().unwrap(),
            connection_type: ConnectionType::DirectTcp,
        }
    }

    fn make_stream_info() -> StreamInfo {
        StreamInfo {
            addr: "127.0.0.1:8080".parse().unwrap(),
            is_tls: false,
            proxy_used: None,
        }
    }

    #[test]
    fn test_pool_per_key_limit() {
        let pool = ConnectionPoolInner::new(2, 100, Duration::from_secs(90));
        let key = make_key();
        let info = make_stream_info();

        pool.return_stream(make_stream(), info.clone());
        pool.return_stream(make_stream(), info.clone());
        pool.return_stream(make_stream(), info.clone()); // should be dropped

        assert_eq!(pool.total_count.load(Ordering::Relaxed), 2);
        assert_eq!(pool.map.get(&key).unwrap().len(), 2);
    }

    #[test]
    fn test_pool_global_limit() {
        let pool = ConnectionPoolInner::new(30, 2, Duration::from_secs(90));
        let info = make_stream_info();

        pool.return_stream(make_stream(), info.clone());
        pool.return_stream(make_stream(), info.clone());
        pool.return_stream(make_stream(), info.clone()); // should be dropped (global limit)

        assert_eq!(pool.total_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_pool_pick_returns_stream() {
        let pool = ConnectionPoolInner::new(30, 100, Duration::from_secs(90));
        let key = make_key();
        let info = make_stream_info();

        pool.return_stream(make_stream(), info);
        let stream = pool.pick(&key);
        assert!(stream.is_some());
        assert_eq!(pool.total_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_pool_pick_returns_none_when_empty() {
        let pool = ConnectionPoolInner::new(30, 100, Duration::from_secs(90));
        let key = make_key();
        assert!(pool.pick(&key).is_none());
    }

    #[test]
    fn test_pool_empty_entry_cleanup() {
        let pool = ConnectionPoolInner::new(30, 100, Duration::from_secs(90));
        let key = make_key();
        let info = make_stream_info();

        pool.return_stream(make_stream(), info);
        assert!(pool.map.contains_key(&key));

        pool.pick(&key);
        assert!(!pool.map.contains_key(&key));
    }

    #[test]
    fn test_pool_idle_eviction_on_return() {
        let pool = ConnectionPoolInner::new(30, 100, Duration::from_millis(1));
        let key = make_key();
        let info = make_stream_info();

        pool.return_stream(make_stream(), info.clone());

        // Insert a stale entry directly to simulate aging
        {
            let mut entry = pool.map.get_mut(&key).unwrap();
            let conn = entry.value_mut().first_mut().unwrap();
            conn.returned_at = Instant::now() - Duration::from_secs(10);
        }

        // Returning a new stream should evict the stale one
        pool.return_stream(make_stream(), info);
        assert_eq!(pool.total_count.load(Ordering::Relaxed), 1);
        assert_eq!(pool.map.get(&key).unwrap().len(), 1);
    }

    #[test]
    fn test_pool_idle_eviction_on_pick() {
        let pool = ConnectionPoolInner::new(30, 100, Duration::from_millis(1));
        let key = make_key();
        let info = make_stream_info();

        pool.return_stream(make_stream(), info);

        // Make the connection appear old
        {
            let mut entry = pool.map.get_mut(&key).unwrap();
            let conn = entry.value_mut().first_mut().unwrap();
            conn.returned_at = Instant::now() - Duration::from_secs(10);
        }

        // Pick should return None (connection evicted as idle)
        let stream = pool.pick(&key);
        assert!(stream.is_none());
        assert!(!pool.map.contains_key(&key));
    }

    #[test]
    fn test_set_pool_config() {
        let client = ZJHttpClient::builder()
            .build()
            .unwrap();
        let client = client.set_pool_config(10, 200, Duration::from_secs(30));
        // Verify pool works with new config
        let info = make_stream_info();
        for _ in 0..10 {
            client.connection_pool.return_stream(make_stream(), info.clone());
        }
        // 11th should be dropped (per-key limit = 10)
        client.connection_pool.return_stream(make_stream(), info);
        assert_eq!(client.connection_pool.total_count.load(Ordering::Relaxed), 10);
    }

}
