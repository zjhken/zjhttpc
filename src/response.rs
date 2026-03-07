use anyhow_ext::{Context, Result, anyhow};
use async_std::io::ReadExt;
use encoding_rs::GBK;
use hashbrown::HashMap;
use indexmap::IndexSet;
use std::{net::SocketAddr, vec};

use tracing::error;

use crate::{
    client::{read_until_v, return_stream_to_pool},
    error::ZjhttpcError,
    misc::HttpVersion,
    proxy::HttpsProxyOption,
    stream::BoxedStream,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A streaming chunked decoder that processes chunks on-the-fly without buffering the entire body
pub struct ChunkedDecoderStream {
    inner: Option<BoxedStream>,
    state: DecoderState,
    chunk_remaining: usize,
    line_buffer: Vec<u8>,
    /// Internal buffer for chunk trailer reading
    trailer_buffer: Vec<u8>,
    /// Shared flag to track if the stream was fully consumed
    completion_flag: Arc<AtomicBool>,
    /// Connection info for returning to pool
    addr: SocketAddr,
    is_tls: bool,
    proxy_used: Option<HttpsProxyOption>,
}

/// A fixed-length stream that tracks remaining bytes and returns 0 when complete
pub struct BodyFixedLengthStream {
    inner: Option<BoxedStream>,
    remaining: usize,
    /// Shared flag to track if the stream was fully consumed
    completion_flag: Arc<AtomicBool>,
    /// Connection info for returning to pool
    addr: SocketAddr,
    is_tls: bool,
    proxy_used: Option<HttpsProxyOption>,
}

/// A stream wrapper for responses with unknown length that returns the stream to pool when EOF is reached
pub struct BodyUnknownLengthStream {
    inner: Option<BoxedStream>,
    /// Shared flag to track if the stream was fully consumed
    completion_flag: Arc<AtomicBool>,
    /// Connection info for returning to pool
    addr: SocketAddr,
    is_tls: bool,
    proxy_used: Option<HttpsProxyOption>,
}

#[derive(Debug, Clone, PartialEq)]
enum DecoderState {
    ReadingChunkSize,
    ReadingChunkData,
    ReadingChunkTrailer,
    Complete,
}

impl ChunkedDecoderStream {
    pub fn new(inner: BoxedStream) -> Self {
        Self {
            inner: Some(inner),
            state: DecoderState::ReadingChunkSize,
            chunk_remaining: 0,
            line_buffer: Vec::new(),
            trailer_buffer: Vec::new(),
            completion_flag: Arc::new(AtomicBool::new(false)),
            addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            is_tls: false,
            proxy_used: None,
        }
    }

    pub fn new_with_completion_flag(
        inner: BoxedStream,
        completion_flag: Arc<AtomicBool>,
        addr: SocketAddr,
        is_tls: bool,
        proxy_used: Option<HttpsProxyOption>,
    ) -> Self {
        Self {
            inner: Some(inner),
            state: DecoderState::ReadingChunkSize,
            chunk_remaining: 0,
            line_buffer: Vec::new(),
            trailer_buffer: Vec::new(),
            completion_flag,
            addr,
            is_tls,
            proxy_used,
        }
    }

    pub fn is_fully_consumed(&self) -> bool {
        self.completion_flag.load(Ordering::Relaxed)
    }

    /// Return the original stream to the connection pool when fully consumed
    fn return_stream_to_pool(&mut self) {
        if let Some(stream) = self.inner.take() {
            let stream_info = crate::client::StreamInfo {
                addr: self.addr,
                is_tls: self.is_tls,
                proxy_used: self.proxy_used.clone(),
            };
            return_stream_to_pool(stream, stream_info);
        }
    }

    /// Try to read the next chunk size. Returns Ok(true) if a new chunk size was read,
    /// Ok(false) if the final chunk (size 0) was reached, or Err if there was an error.
    async fn read_chunk_size(&mut self) -> Result<bool> {
        let inner_stream = self
            .inner
            .as_mut()
            .ok_or_else(|| anyhow!("stream is None"))?;

        self.line_buffer.clear();
        let n = read_until_v(inner_stream, b"\r\n", &mut self.line_buffer).await?;

        let mut chunk_size_str = String::from_utf8_lossy(&self.line_buffer[..n]);
        // Sometimes there will be \r\n in the beginning instead of the number
        if chunk_size_str.trim().is_empty() {
            self.line_buffer.clear();
            let n = read_until_v(inner_stream, b"\r\n", &mut self.line_buffer).await?;
            chunk_size_str = String::from_utf8_lossy(&self.line_buffer[..n]);
        }

        let chunk_size = usize::from_str_radix(chunk_size_str.trim(), 16).map_err(|e| {
            anyhow!(
                "invalid chunk size '{:?}': {}",
                chunk_size_str.as_bytes(),
                e
            )
        })?;

        if chunk_size == 0 {
            // Read the trailing \r\n after the final chunk size
            self.line_buffer.clear();
            let n = read_until_v(inner_stream, b"\r\n", &mut self.line_buffer).await?;
            if n != 2 {
                let x = String::from_utf8_lossy(&self.line_buffer[..n]);
                return Err(anyhow!(
                    "not possible, it's not \\r\\n after zero in chunk. x={x}, n={n}"
                ));
            }
            return Ok(false); // Final chunk reached
        }

        self.chunk_remaining = chunk_size;
        Ok(true) // New chunk size read successfully
    }

    /// Read chunk trailer (the \r\n after chunk data)
    async fn read_chunk_trailer(&mut self) -> Result<()> {
        let inner_stream = self
            .inner
            .as_mut()
            .ok_or_else(|| anyhow!("stream is None"))?;

        self.trailer_buffer.clear();
        let n = read_until_v(inner_stream, b"\r\n", &mut self.trailer_buffer).await?;
        if n != 2 {
            let x = String::from_utf8_lossy(&self.trailer_buffer[..n]);
            return Err(anyhow!(
                "not possible, it's not \\r\\n after chunk data. x={x}, n={n}"
            ));
        }
        Ok(())
    }
}

impl async_std::io::Read for ChunkedDecoderStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.completion_flag.load(Ordering::Relaxed) {
            return std::task::Poll::Ready(Ok(0));
        }

        loop {
            match &self.state {
                DecoderState::Complete => {
                    self.completion_flag.store(true, Ordering::Relaxed);
                    self.return_stream_to_pool();
                    return std::task::Poll::Ready(Ok(0));
                }
                DecoderState::ReadingChunkSize => {
                    // Try to read the next chunk size
                    match async_std::task::block_on(self.read_chunk_size()) {
                        Ok(true) => {
                            // Got a new chunk size, switch to reading data
                            self.state = DecoderState::ReadingChunkData;
                            continue;
                        }
                        Ok(false) => {
                            // Final chunk reached, we're done
                            self.state = DecoderState::Complete;
                            self.completion_flag.store(true, Ordering::Relaxed);
                            self.return_stream_to_pool();
                            return std::task::Poll::Ready(Ok(0));
                        }
                        Err(e) => {
                            return std::task::Poll::Ready(Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                e,
                            )));
                        }
                    }
                }
                DecoderState::ReadingChunkTrailer => {
                    // Read the trailer after chunk data
                    match async_std::task::block_on(self.read_chunk_trailer()) {
                        Ok(_) => {
                            // Trailer read successfully, go back to reading next chunk size
                            self.state = DecoderState::ReadingChunkSize;
                            continue;
                        }
                        Err(e) => {
                            return std::task::Poll::Ready(Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                e,
                            )));
                        }
                    }
                }
                DecoderState::ReadingChunkData => {
                    // If we have no more data in this chunk, move to trailer reading
                    if self.chunk_remaining == 0 {
                        self.state = DecoderState::ReadingChunkTrailer;
                        continue;
                    }

                    // Read data from the current chunk
                    let to_read = std::cmp::min(buf.len(), self.chunk_remaining);
                    let mut temp_buf = vec![0u8; to_read];

                    if let Some(inner_stream) = &mut self.inner {
                        match std::pin::Pin::new(inner_stream).poll_read(cx, &mut temp_buf) {
                            std::task::Poll::Ready(Ok(n)) => {
                                if n == 0 {
                                    return std::task::Poll::Ready(Err(std::io::Error::new(
                                        std::io::ErrorKind::UnexpectedEof,
                                        "unexpected end of stream while reading chunk data",
                                    )));
                                }

                                buf[..n].copy_from_slice(&temp_buf[..n]);
                                self.chunk_remaining -= n;

                                return std::task::Poll::Ready(Ok(n));
                            }
                            std::task::Poll::Ready(Err(e)) => {
                                return std::task::Poll::Ready(Err(e));
                            }
                            std::task::Poll::Pending => return std::task::Poll::Pending,
                        }
                    } else {
                        return std::task::Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            "stream is None",
                        )));
                    }
                }
            }
        }
    }
}

impl async_std::io::Write for ChunkedDecoderStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "ChunkedDecoderStream is read-only",
        )))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl BodyFixedLengthStream {
    pub fn new(inner: BoxedStream, content_length: usize) -> Self {
        Self {
            inner: Some(inner),
            remaining: content_length,
            completion_flag: Arc::new(AtomicBool::new(false)),
            addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            is_tls: false,
            proxy_used: None,
        }
    }

    pub fn new_with_completion_flag(
        inner: BoxedStream,
        content_length: usize,
        completion_flag: Arc<AtomicBool>,
        addr: SocketAddr,
        is_tls: bool,
        proxy_used: Option<HttpsProxyOption>,
    ) -> Self {
        Self {
            inner: Some(inner),
            remaining: content_length,
            completion_flag,
            addr,
            is_tls,
            proxy_used,
        }
    }

    pub fn is_fully_consumed(&self) -> bool {
        self.completion_flag.load(Ordering::Relaxed)
    }

    /// Return the original stream to the connection pool when fully consumed
    fn return_stream_to_pool(&mut self) {
        if let Some(stream) = self.inner.take() {
            let stream_info = crate::client::StreamInfo {
                addr: self.addr,
                is_tls: self.is_tls,
                proxy_used: self.proxy_used.clone(),
            };
            return_stream_to_pool(stream, stream_info);
        }
    }
}

impl async_std::io::Read for BodyFixedLengthStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.completion_flag.load(Ordering::Relaxed) || self.remaining == 0 {
            self.completion_flag.store(true, Ordering::Relaxed);
            self.return_stream_to_pool();
            return std::task::Poll::Ready(Ok(0));
        }

        let to_read = std::cmp::min(buf.len(), self.remaining);
        if to_read == 0 {
            self.completion_flag.store(true, Ordering::Relaxed);
            self.return_stream_to_pool();
            return std::task::Poll::Ready(Ok(0));
        }

        if let Some(inner_stream) = &mut self.inner {
            match std::pin::Pin::new(inner_stream).poll_read(cx, &mut buf[..to_read]) {
                std::task::Poll::Ready(Ok(n)) => {
                    if n == 0 {
                        return std::task::Poll::Ready(Ok(0));
                    }

                    self.remaining -= n;
                    if self.remaining == 0 {
                        self.completion_flag.store(true, Ordering::Relaxed);
                        self.return_stream_to_pool();
                    }

                    std::task::Poll::Ready(Ok(n))
                }
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        } else {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "stream is None",
            )));
        }
    }
}

impl async_std::io::Write for BodyFixedLengthStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "BodyFixedLengthStream is read-only",
        )))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl BodyUnknownLengthStream {
    pub fn new_with_completion_flag(
        inner: BoxedStream,
        completion_flag: Arc<AtomicBool>,
        addr: SocketAddr,
        is_tls: bool,
        proxy_used: Option<HttpsProxyOption>,
    ) -> Self {
        Self {
            inner: Some(inner),
            completion_flag,
            addr,
            is_tls,
            proxy_used,
        }
    }

    pub fn is_fully_consumed(&self) -> bool {
        self.completion_flag.load(Ordering::Relaxed)
    }

    /// Return the original stream to the connection pool when fully consumed
    fn return_stream_to_pool(&mut self) {
        if let Some(stream) = self.inner.take() {
            let stream_info = crate::client::StreamInfo {
                addr: self.addr,
                is_tls: self.is_tls,
                proxy_used: self.proxy_used.clone(),
            };
            return_stream_to_pool(stream, stream_info);
        }
    }
}

impl async_std::io::Read for BodyUnknownLengthStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.completion_flag.load(Ordering::Relaxed) {
            return std::task::Poll::Ready(Ok(0));
        }

        if let Some(inner_stream) = &mut self.inner {
            match std::pin::Pin::new(inner_stream).poll_read(cx, buf) {
                std::task::Poll::Ready(Ok(0)) => {
                    // EOF reached - mark as completed and return to pool
                    self.completion_flag.store(true, Ordering::Relaxed);
                    self.return_stream_to_pool();
                    std::task::Poll::Ready(Ok(0))
                }
                std::task::Poll::Ready(Ok(n)) => std::task::Poll::Ready(Ok(n)),
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        } else {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "stream is None",
            )))
        }
    }
}

impl async_std::io::Write for BodyUnknownLengthStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "BodyUnknownLengthStream is read-only",
        )))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl crate::stream::RWStream for BodyFixedLengthStream {}

impl crate::stream::RWStream for ChunkedDecoderStream {}

impl crate::stream::RWStream for BodyUnknownLengthStream {}

pub struct Response {
    pub addr: SocketAddr,
    pub is_tls: bool,
    pub http_version: HttpVersion,
    pub status_code: u16,
    pub headers: HashMap<String, IndexSet<String>>,
    /// If you use this raw stream directly, call mark_body_read_complete() when done
    /// If you use body_managed_stream() instead, the returned wrapper handles this automatically
    pub body_raw_stream: Option<BoxedStream>,
    pub proxy_used: Option<HttpsProxyOption>,
    /// Track if the response body has been fully consumed
    /// This is used to determine if the connection should be returned to pool on Drop
    /// - For managed streams: wrapper sets this to true when fully consumed
    /// - For raw streams: user should call mark_body_read_complete() when done
    body_completion_flag: Arc<AtomicBool>,
    /// Timeout for reading response body
    pub read_body_timeout: Option<std::time::Duration>,
}

impl Drop for Response {
    fn drop(&mut self) {
        // Check if body was fully consumed
        if self.body_completion_flag.load(Ordering::Relaxed) {
            // Body was fully consumed
            // If body_raw_stream is still present (user used raw_stream + mark_complete), return to pool
            if let Some(stream) = self.body_raw_stream.take() {
                let stream_info = crate::client::StreamInfo {
                    addr: self.addr,
                    is_tls: self.is_tls,
                    proxy_used: self.proxy_used.clone(),
                };
                return_stream_to_pool(stream, stream_info);
            }
            // If body_raw_stream is None, managed stream already returned it
        } else {
            // Body was NOT fully consumed
            // Do NOT return to pool to avoid contamination
            // The connection will be closed when the stream is dropped
        }
    }
}

impl Response {
    pub fn new_from_parse_result(
        http_version: &str,
        status_code: &str,
        headers_vec: Vec<(String, String)>,
        stream: BoxedStream,
        is_tls: bool,
        addr: SocketAddr,
        proxy_used: Option<HttpsProxyOption>,
        read_body_timeout: Option<std::time::Duration>,
    ) -> Result<Self, ZjhttpcError> {
        let http_version = match http_version {
            "1.1" => HttpVersion::V1_1,
            "1.0" => HttpVersion::V1_0,
            others => return Err(ZjhttpcError::InvalidHttpResponseVersion(others.to_string())),
        };
        let status_code: u16 = status_code
            .parse()
            .map_err(|_e| ZjhttpcError::InvalidHttpResponseStatusCode(status_code.to_string()))?;
        let mut headers: HashMap<String, IndexSet<String>> = HashMap::new();
        for (key, value) in headers_vec {
            match headers.get_mut(&key) {
                Some(set) => {
                    set.insert(value);
                }
                None => {
                    let mut set = IndexSet::new();
                    set.insert(value);
                    headers.insert(key, set);
                }
            };
        }
        let resp = Response {
            is_tls,
            http_version,
            status_code,
            headers,
            body_raw_stream: Some(stream),
            addr,
            proxy_used,
            body_completion_flag: Arc::new(AtomicBool::new(false)),
            read_body_timeout,
        };
        return Ok(resp);
    }
    pub fn status_code(&self) -> u16 {
        self.status_code
    }

    pub fn is_success(&self) -> bool {
        (200u16..300u16).contains(&self.status_code)
    }

    pub fn header_one(&self, header_name: impl AsRef<str>) -> Option<&str> {
        self.headers
            .get(&header_name.as_ref().to_ascii_lowercase())
            .map(|x| x.first().map(|x| x.as_str()))
            .flatten()
    }

    pub fn header_all(&self, key: impl AsRef<str>) -> Vec<&str> {
        let key = key.as_ref().to_ascii_lowercase();
        self.headers
            .get(&key)
            .map(|set| set.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Read cookies from Set-Cookie headers
    ///
    /// # Returns
    /// Vec of cookies parsed from all Set-Cookie headers
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::response::Response;
    /// use zjhttpc::cookie::Cookie;
    ///
    /// // Assuming you have a Response instance
    /// let cookies = response.read_cookies();
    /// for cookie in cookies {
    ///     println!("Cookie: {}={}", cookie.name, cookie.value);
    /// }
    /// ```
    pub fn read_cookies(&self) -> Vec<crate::cookie::Cookie> {
        self.header_all(crate::header::SET_COOKIE)
            .iter()
            .flat_map(|&value| crate::cookie::Cookie::parse_from_set_cookie(std::iter::once(value)))
            .collect()
    }

    pub async fn body_string(&mut self) -> Result<String> {
        if self.is_body_read_complete() {
            return Err(anyhow!("response body has been read"));
        }

        if let Some(mut stream) = self.body_managed_stream() {
            let mut bytes: Vec<u8> = Vec::new();
            let mut buf = [0u8; 1024];

            // Apply read body timeout if set
            let read_future = async {
                while let n = stream.read(&mut buf).await.dot()?
                    && n > 0
                {
                    bytes.extend_from_slice(&buf[..n]);
                }
                Ok::<(), anyhow::Error>(())
            };

            if let Some(timeout) = self.read_body_timeout {
                async_std::future::timeout(timeout, read_future)
                    .await
                    .map_err(|_| anyhow!("read body timeout after {:?}", timeout))??;
            } else {
                read_future.await?;
            }

            // considering the encoding
            if let Some(x) = self.headers.get("content-type")
                && x.last()
                    .map(|x| x.to_lowercase().contains("charset=gbk"))
                    .unwrap_or(false)
            {
                let (cow, _encoding, had_errors) = GBK.decode(&bytes.as_slice());
                if had_errors {
                    error!("GBK decode with errors");
                }
                return Ok(cow.to_string());
            } else {
                return Ok(String::from_utf8_lossy(&bytes).to_string());
            }
        } else {
            return Ok(String::new());
        }
    }

    /// Returns a streaming response body with automatic completion tracking.
    ///
    /// This function provides true streaming with proper connection pool management:
    /// - For chunked responses, it decodes chunks on-the-fly without buffering the entire body in memory
    /// - For responses with Content-Length, it returns a fixed-length stream that tracks remaining bytes  
    /// - For other responses, it wraps the raw stream in BodyUnknownLengthStream for completion tracking
    ///
    /// # Important Notes
    ///
    /// - Returns `None` if the body has already been read via `body_string()` or other methods.
    /// - For chunked transfer encoding responses, automatically decodes chunks as you read.
    /// - For responses with Content-Length header, returns a BodyFixedLengthStream that tracks remaining bytes.
    /// - For other responses, returns a BodyUnknownLengthStream that detects EOF and returns the connection to pool.
    /// - All wrapper streams automatically return the connection to the pool when fully consumed (EOF reached).
    /// - Once you use this stream, you become responsible for reading it completely.
    /// - If you don't read the stream completely, the connection may not be reusable.
    pub fn body_managed_stream(&mut self) -> Option<BoxedStream> {
        if self.is_body_read_complete() {
            return None;
        }

        // Check if this is chunked encoding
        let is_chunked = self
            .headers
            .get("transfer-encoding")
            .map(|set| set.iter().any(|v| v.contains("chunked")))
            .unwrap_or(false);

        // Check if Content-Length is present
        let content_length = self.content_length();

        if let Some(stream) = self.body_raw_stream.take() {
            if is_chunked {
                // For chunked encoding, wrap the stream with our decoder
                let decoder = ChunkedDecoderStream::new_with_completion_flag(
                    stream,
                    self.body_completion_flag.clone(),
                    self.addr,
                    self.is_tls,
                    self.proxy_used.clone(),
                );
                Some(Box::new(decoder))
            } else if let Some(length) = content_length {
                // For responses with Content-Length, use BodyFixedLengthStream
                let fixed_length_stream = BodyFixedLengthStream::new_with_completion_flag(
                    stream,
                    length as usize,
                    self.body_completion_flag.clone(),
                    self.addr,
                    self.is_tls,
                    self.proxy_used.clone(),
                );
                Some(Box::new(fixed_length_stream))
            } else {
                // For other responses, wrap with BodyUnknownLengthStream for completion tracking
                let unknown_length_stream = BodyUnknownLengthStream::new_with_completion_flag(
                    stream,
                    self.body_completion_flag.clone(),
                    self.addr,
                    self.is_tls,
                    self.proxy_used.clone(),
                );
                Some(Box::new(unknown_length_stream))
            }
        } else {
            None
        }
    }

    /// Read the entire body and return it as bytes
    ///
    /// This method consumes the response body and reads all data into memory.
    /// For large bodies, consider using body_managed_stream() for streaming access.
    pub async fn body_bytes(&mut self) -> Result<Vec<u8>> {
        if self.is_body_read_complete() {
            return Err(anyhow!("response body has been read"));
        }

        if let Some(mut stream) = self.body_managed_stream() {
            let mut bytes: Vec<u8> = Vec::new();
            let mut buf = [0u8; 8192]; // 8KB buffer

            // Apply read body timeout if set
            let read_future = async {
                while let n = stream.read(&mut buf).await.dot()?
                    && n > 0
                {
                    bytes.extend_from_slice(&buf[..n]);
                }
                Ok::<(), anyhow::Error>(())
            };

            if let Some(timeout) = self.read_body_timeout {
                async_std::future::timeout(timeout, read_future)
                    .await
                    .map_err(|_| anyhow!("read body timeout after {:?}", timeout))??;
            } else {
                read_future.await?;
            }

            Ok(bytes)
        } else {
            Ok(Vec::new())
        }
    }

    // reading the entire body and return a JSON object
    pub async fn body_json(&mut self) -> Result<serde_json::Value> {
        let bytes = self.body_bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| anyhow!("JSON parsing failed: {}", e))
    }

    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get("content-length")
            .and_then(|vec| vec.first())
            .and_then(|s| s.parse::<u64>().ok())
    }

    /// Mark the response body as successfully read.
    ///
    /// This method should be called when you have finished reading the body through
    /// `body_raw_stream` directly. It ensures the connection can be returned to the pool
    /// for reuse.
    ///
    /// # When to use this
    ///
    /// - **Use this** when you read from `body_raw_stream` directly
    /// - **Don't use this** when you use `body_managed_stream()`, `body_bytes()`, or `body_string()` -
    ///   they handle completion tracking automatically
    ///
    /// # Example
    ///
    /// ```rust
    /// let mut resp = client.send(&mut req).await?;
    /// if let Some(mut stream) = resp.body_raw_stream.take() {
    ///     // Read data...
    ///     let mut buf = [0u8; 1024];
    ///     while let Ok(n) = stream.read(&mut buf).await {
    ///         if n == 0 { break; }
    ///         // Process data...
    ///     }
    ///     // Mark as complete so connection can be reused
    ///     resp.mark_body_read_complete();
    /// }
    /// ```
    pub fn mark_body_read_complete(&mut self) {
        self.body_completion_flag.store(true, Ordering::Relaxed);
    }

    /// Check if the response body has been successfully read.
    ///
    /// Returns `true` if:
    /// - The body was read via `body_managed_stream()` and fully consumed, OR
    /// - The body was read via `body_raw_stream` and `mark_body_read_complete()` was called
    pub fn is_body_read_complete(&self) -> bool {
        self.body_completion_flag.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use async_std::task;

    use crate::{client::ZJHttpClient, requestx::Request};

    use super::*;

    #[test]
    fn new_from_parse_result_and_basic_getters() {
        let x = "\r\nf5e\r\n".trim();
        println!("{x}");
    }

    #[test]
    #[tracing_test::traced_test]
    #[ignore] // This test requires a local HTTP server running on 127.0.0.1:8888
    fn test_chunked() {
        task::block_on(async {
            // let mut req = Request::new("GET", "http://127.0.0.1:8888/test/chunk").unwrap();
            let mut req = Request::new("GET", "http://127.0.0.1:8888/test/gb2312.txt").unwrap();
            let mut resp = ZJHttpClient::builder().build().unwrap().send(&mut req).await.unwrap();
            let s = resp.body_string().await.unwrap();
            println!("{}", s);
        });
    }

    #[test]
    fn test_body_stream_basic() {
        // Test that body_stream returns None when body has been read
        let x = "\r\nf5e\r\n".trim();
        println!("{x}");

        // This is a basic test - in a real scenario you'd need a proper Response struct
        // with a body_stream field initialized
    }

    #[test]
    fn test_body_fixed_length_stream() {
        use async_std::io::ReadExt;

        // Create a simple test stream that implements RWStream
        struct TestStream {
            data: Vec<u8>,
            position: usize,
        }

        impl async_std::io::Read for TestStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestStream {}

        // Create a mock stream with some data
        let data = b"Hello, World!";
        let test_stream = TestStream {
            data: data.to_vec(),
            position: 0,
        };
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        // Create BodyFixedLengthStream with exact content length
        let mut fixed_stream = BodyFixedLengthStream::new(boxed_stream, data.len());

        // Test reading the entire content
        let mut buffer = Vec::new();
        let result = async_std::task::block_on(fixed_stream.read_to_end(&mut buffer));

        assert!(result.is_ok());
        assert_eq!(buffer, data);

        // Test that subsequent reads return 0 (EOF)
        let mut small_buffer = [0u8; 10];
        let result = async_std::task::block_on(fixed_stream.read(&mut small_buffer));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_body_fixed_length_stream_partial_read() {
        use async_std::io::ReadExt;

        // Create a simple test stream that implements RWStream
        struct TestStream {
            data: Vec<u8>,
            position: usize,
        }

        impl async_std::io::Read for TestStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestStream {}

        // Create a mock stream with some data
        let data = b"Hello, World!";
        let test_stream = TestStream {
            data: data.to_vec(),
            position: 0,
        };
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        // Create BodyFixedLengthStream with exact content length
        let mut fixed_stream = BodyFixedLengthStream::new(boxed_stream, data.len());

        // Test reading partial content
        let mut buffer = [0u8; 5];
        let result = async_std::task::block_on(fixed_stream.read(&mut buffer));

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 5);
        assert_eq!(&buffer[..5], b"Hello");

        // Read the rest
        let mut remaining_buffer = Vec::new();
        let result = async_std::task::block_on(fixed_stream.read_to_end(&mut remaining_buffer));

        assert!(result.is_ok());
        assert_eq!(remaining_buffer, b", World!");
    }

    #[test]
    fn test_gb2312_decoding() {
        let bytes = include_bytes!("/Users/bluewater/codes/stock-noti/dev/gb2312.txt");
        let (a, _b, _c) = encoding_rs::GBK.decode(bytes);
        println!("{}", a.to_string());
    }

    #[test]
    fn test_chunked_decoder_stream_basic() {
        use async_std::io::ReadExt;

        // Create a test stream that simulates chunked encoded data
        struct TestChunkedStream {
            data: Vec<u8>,
            position: usize,
        }

        impl TestChunkedStream {
            fn new(chunked_data: &[u8]) -> Self {
                Self {
                    data: chunked_data.to_vec(),
                    position: 0,
                }
            }
        }

        impl async_std::io::Read for TestChunkedStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestChunkedStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestChunkedStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestChunkedStream {}

        // Create chunked data: "5\r\nHello\r\n6\r\n World\r\n0\r\n\r\n"
        let chunked_data = b"5\r\nHello\r\n6\r\n World\r\n0\r\n\r\n";
        let test_stream = TestChunkedStream::new(chunked_data);
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        // Test ChunkedDecoderStream
        let mut decoder = ChunkedDecoderStream::new(boxed_stream);

        // Read all data
        let mut buffer = Vec::new();
        let result = async_std::task::block_on(decoder.read_to_end(&mut buffer));

        assert!(result.is_ok());
        assert_eq!(buffer, b"Hello World");
        assert!(decoder.is_fully_consumed());
    }

    #[test]
    fn test_body_unknown_length_stream_basic() {
        use async_std::io::ReadExt;

        // Create a simple test stream
        struct TestStream {
            data: Vec<u8>,
            position: usize,
        }

        impl TestStream {
            fn new(data: &[u8]) -> Self {
                Self {
                    data: data.to_vec(),
                    position: 0,
                }
            }
        }

        impl async_std::io::Read for TestStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestStream {}

        // Test with some data
        let data = b"Test data for unknown length stream";
        let test_stream = TestStream::new(data);
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        // Create BodyUnknownLengthStream
        let completion_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut unknown_stream = BodyUnknownLengthStream::new_with_completion_flag(
            boxed_stream,
            completion_flag,
            std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
            false,
            None,
        );

        // Read all data
        let mut buffer = Vec::new();
        let result = async_std::task::block_on(unknown_stream.read_to_end(&mut buffer));

        assert!(result.is_ok());
        assert_eq!(buffer, data);
        assert!(unknown_stream.is_fully_consumed());
    }

    #[test]
    fn test_response_chunked_encoding_detection() {
        use hashbrown::HashMap;
        use indexmap::IndexSet;

        // Create a response with chunked encoding
        let mut headers = HashMap::new();
        let mut transfer_encoding_set = IndexSet::new();
        transfer_encoding_set.insert("chunked".to_string());
        headers.insert("transfer-encoding".to_string(), transfer_encoding_set);

        // Test chunked detection logic
        let is_chunked = headers
            .get("transfer-encoding")
            .map(|set| set.iter().any(|v| v.contains("chunked")))
            .unwrap_or(false);

        assert!(is_chunked);

        // Test non-chunked response
        let mut headers2 = HashMap::new();
        let mut content_length_set = IndexSet::new();
        content_length_set.insert("123".to_string());
        headers2.insert("content-length".to_string(), content_length_set);

        let is_chunked2 = headers2
            .get("transfer-encoding")
            .map(|set| set.iter().any(|v| v.contains("chunked")))
            .unwrap_or(false);

        assert!(!is_chunked2);
    }

    #[test]
    fn test_response_content_length_parsing() {
        use hashbrown::HashMap;
        use indexmap::IndexSet;

        // Create a response with Content-Length header
        let mut headers = HashMap::new();
        let mut content_length_set = IndexSet::new();
        content_length_set.insert("1024".to_string());
        headers.insert("content-length".to_string(), content_length_set);

        // Test Content-Length parsing logic
        let content_length = headers
            .get("content-length")
            .and_then(|vec| vec.first())
            .and_then(|s| s.parse::<u64>().ok());

        assert_eq!(content_length, Some(1024u64));

        // Test with invalid Content-Length
        let mut headers2 = HashMap::new();
        let mut content_length_set2 = IndexSet::new();
        content_length_set2.insert("invalid".to_string());
        headers2.insert("content-length".to_string(), content_length_set2);

        let content_length2 = headers2
            .get("content-length")
            .and_then(|vec| vec.first())
            .and_then(|s| s.parse::<u64>().ok());

        assert_eq!(content_length2, None);

        // Test with no Content-Length header
        let content_length3: Option<u64> = None;
        assert_eq!(content_length3, None);
    }

    #[test]
    fn test_body_stream_completion_flag_behavior() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // Test completion flag behavior
        let completion_flag = Arc::new(AtomicBool::new(false));

        // Initially should be false
        assert!(!completion_flag.load(Ordering::Relaxed));

        // Set to true
        completion_flag.store(true, Ordering::Relaxed);
        assert!(completion_flag.load(Ordering::Relaxed));
    }

    #[test]
    fn test_response_body_successfully_readed_flag() {
        use hashbrown::HashMap;
        use std::net::SocketAddr;

        // Create a mock response
        let response = Response {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            is_tls: false,
            http_version: HttpVersion::V1_1,
            status_code: 200,
            headers: HashMap::new(),
            body_raw_stream: None,
            proxy_used: None,
            body_completion_flag: Arc::new(AtomicBool::new(false)),
            read_body_timeout: None,
        };

        // Test initial state
        assert!(!response.is_body_read_complete());
        assert_eq!(response.status_code(), 200);
        assert!(response.is_success());
    }

    #[test]
    fn test_mark_body_read_complete() {
        use hashbrown::HashMap;
        use std::net::SocketAddr;

        // Create a mock response with raw_stream
        let mut response = Response {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            is_tls: false,
            http_version: HttpVersion::V1_1,
            status_code: 200,
            headers: HashMap::new(),
            body_raw_stream: None,
            proxy_used: None,
            body_completion_flag: Arc::new(AtomicBool::new(false)),
            read_body_timeout: None,
        };

        // Initially not complete
        assert!(!response.is_body_read_complete());

        // Mark as complete
        response.mark_body_read_complete();

        // Now should be complete
        assert!(response.is_body_read_complete());
    }

    #[test]
    fn test_completion_flag_with_managed_stream() {
        use hashbrown::HashMap;
        use std::net::SocketAddr;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // Test that the same completion_flag is shared between Response and wrapper
        let completion_flag = Arc::new(AtomicBool::new(false));
        let response = Response {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            is_tls: false,
            http_version: HttpVersion::V1_1,
            status_code: 200,
            headers: HashMap::new(),
            body_raw_stream: None,
            proxy_used: None,
            body_completion_flag: completion_flag.clone(),
            read_body_timeout: None,
        };

        // Initially not complete
        assert!(!response.is_body_read_complete());

        // Simulate managed stream completing (wrapper sets flag to true)
        response.body_completion_flag.store(true, Ordering::Relaxed);

        // Response should see the change
        assert!(response.is_body_read_complete());
    }

    #[test]
    fn test_body_fixed_length_stream_zero_length() {
        use async_std::io::ReadExt;

        // Create a test stream with some data
        struct TestStream {
            data: Vec<u8>,
            position: usize,
        }

        impl async_std::io::Read for TestStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestStream {}

        // Create a mock stream with some data
        let data = b"Some data";
        let test_stream = TestStream {
            data: data.to_vec(),
            position: 0,
        };
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        // Create BodyFixedLengthStream with zero content length
        let mut fixed_stream = BodyFixedLengthStream::new(boxed_stream, 0);

        // Test that reading returns 0 immediately
        let mut buffer = [0u8; 10];
        let result = async_std::task::block_on(fixed_stream.read(&mut buffer));

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
        assert!(fixed_stream.is_fully_consumed());
    }

    #[test]
    fn test_body_bytes_method() {
        use async_std::io::ReadExt;

        // Create a test stream
        struct TestStream {
            data: Vec<u8>,
            position: usize,
        }

        impl TestStream {
            fn new(data: &[u8]) -> Self {
                Self {
                    data: data.to_vec(),
                    position: 0,
                }
            }
        }

        impl async_std::io::Read for TestStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestStream {}

        // Test data
        let data = b"Hello, World! This is test data for body_bytes method.";
        let test_stream = TestStream::new(data);
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        // Create a response with content-length
        let mut headers = hashbrown::HashMap::new();
        let mut content_length_set = indexmap::IndexSet::new();
        content_length_set.insert(data.len().to_string());
        headers.insert("content-length".to_string(), content_length_set);

        let mut response = Response {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
            is_tls: false,
            http_version: HttpVersion::V1_1,
            status_code: 200,
            headers,
            body_raw_stream: Some(boxed_stream),
            proxy_used: None,
            body_completion_flag: Arc::new(AtomicBool::new(false)),
            read_body_timeout: None,
        };

        // Test body_bytes method
        let result = async_std::task::block_on(response.body_bytes());
        assert!(result.is_ok());
        let bytes = result.unwrap();
        assert_eq!(bytes, data);
    }

    #[test]
    fn test_body_json_method() {
        use async_std::io::ReadExt;

        // Create a test stream with JSON data
        struct TestStream {
            data: Vec<u8>,
            position: usize,
        }

        impl TestStream {
            fn new(data: &[u8]) -> Self {
                Self {
                    data: data.to_vec(),
                    position: 0,
                }
            }
        }

        impl async_std::io::Read for TestStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestStream {}

        // Test JSON data
        let json_data = br#"{"name": "test", "value": 42, "active": true}"#;
        let test_stream = TestStream::new(json_data);
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        // Create a response with JSON content
        let mut headers = hashbrown::HashMap::new();
        let mut content_length_set = indexmap::IndexSet::new();
        content_length_set.insert(json_data.len().to_string());
        headers.insert("content-length".to_string(), content_length_set);

        let mut response = Response {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
            is_tls: false,
            http_version: HttpVersion::V1_1,
            status_code: 200,
            headers,
            body_raw_stream: Some(boxed_stream),
            proxy_used: None,
            body_completion_flag: Arc::new(AtomicBool::new(false)),
            read_body_timeout: None,
        };

        // Test body_json method
        let result = async_std::task::block_on(response.body_json());
        assert!(result.is_ok());
        let json_value = result.unwrap();

        // Verify JSON parsing
        assert_eq!(json_value["name"], "test");
        assert_eq!(json_value["value"], 42);
        assert_eq!(json_value["active"], true);
    }

    #[test]
    fn test_body_json_invalid_json() {
        use async_std::io::ReadExt;

        // Create a test stream with invalid JSON data
        struct TestStream {
            data: Vec<u8>,
            position: usize,
        }

        impl TestStream {
            fn new(data: &[u8]) -> Self {
                Self {
                    data: data.to_vec(),
                    position: 0,
                }
            }
        }

        impl async_std::io::Read for TestStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.position;
                let to_read = std::cmp::min(buf.len(), remaining);

                if to_read > 0 {
                    buf[..to_read]
                        .copy_from_slice(&self.data[self.position..self.position + to_read]);
                    self.position += to_read;
                }

                std::task::Poll::Ready(Ok(to_read))
            }
        }

        impl async_std::io::Write for TestStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "TestStream is read-only",
                )))
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        impl crate::stream::RWStream for TestStream {}

        // Test invalid JSON data
        let invalid_json = b"{ invalid json }";
        let test_stream = TestStream::new(invalid_json);
        let boxed_stream = Box::new(test_stream) as BoxedStream;

        let mut response = Response {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
            is_tls: false,
            http_version: HttpVersion::V1_1,
            status_code: 200,
            headers: hashbrown::HashMap::new(),
            body_raw_stream: Some(boxed_stream),
            proxy_used: None,
            body_completion_flag: Arc::new(AtomicBool::new(false)),
            read_body_timeout: None,
        };

        // Test body_json method with invalid JSON
        let result = async_std::task::block_on(response.body_json());
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("JSON parsing failed"));
    }
}
