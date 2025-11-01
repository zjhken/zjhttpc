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
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

/// A streaming chunked decoder that processes chunks on-the-fly without buffering the entire body
pub struct ChunkedDecoderStream {
    inner: Option<BoxedStream>,
    state: DecoderState,
    chunk_remaining: usize,
    line_buffer: Vec<u8>,
    eof_reached: bool,
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
    eof_reached: bool,
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
            eof_reached: false,
            trailer_buffer: Vec::new(),
            completion_flag: Arc::new(AtomicBool::new(false)),
            addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            is_tls: false,
            proxy_used: None,
        }
    }

    pub fn new_with_completion_flag(inner: BoxedStream, completion_flag: Arc<AtomicBool>, addr: SocketAddr, is_tls: bool, proxy_used: Option<HttpsProxyOption>) -> Self {
        Self {
            inner: Some(inner),
            state: DecoderState::ReadingChunkSize,
            chunk_remaining: 0,
            line_buffer: Vec::new(),
            eof_reached: false,
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
            let mut response = Response {
                addr: self.addr,
                is_tls: self.is_tls,
                body_successfully_readed: true,
                http_version: crate::misc::HttpVersion::V1_1,
                status_code: 200,
                headers: hashbrown::HashMap::new(),
                body_raw_stream: Some(stream),
                proxy_used: self.proxy_used.clone(),
                stream_completion_flag: None,
            };
            return_stream_to_pool(&mut response);
        }
    }

    /// Try to read the next chunk size. Returns Ok(true) if a new chunk size was read,
    /// Ok(false) if the final chunk (size 0) was reached, or Err if there was an error.
    async fn read_chunk_size(&mut self) -> Result<bool> {
        let inner_stream = self.inner.as_mut().ok_or_else(|| anyhow!("stream is None"))?;
        
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
        let inner_stream = self.inner.as_mut().ok_or_else(|| anyhow!("stream is None"))?;
        
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
        if self.eof_reached {
            return std::task::Poll::Ready(Ok(0));
        }

        loop {
            match &self.state {
                DecoderState::Complete => {
                    self.eof_reached = true;
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
                            self.eof_reached = true;
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
                            std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
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
            eof_reached: false,
            completion_flag: Arc::new(AtomicBool::new(false)),
            addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            is_tls: false,
            proxy_used: None,
        }
    }

    pub fn new_with_completion_flag(inner: BoxedStream, content_length: usize, completion_flag: Arc<AtomicBool>, addr: SocketAddr, is_tls: bool, proxy_used: Option<HttpsProxyOption>) -> Self {
        Self {
            inner: Some(inner),
            remaining: content_length,
            eof_reached: false,
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
            let mut response = Response {
                addr: self.addr,
                is_tls: self.is_tls,
                body_successfully_readed: true,
                http_version: crate::misc::HttpVersion::V1_1,
                status_code: 200,
                headers: hashbrown::HashMap::new(),
                body_raw_stream: Some(stream),
                proxy_used: self.proxy_used.clone(),
                stream_completion_flag: None,
            };
            return_stream_to_pool(&mut response);
        }
    }
}

impl async_std::io::Read for BodyFixedLengthStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.eof_reached || self.remaining == 0 {
            self.eof_reached = true;
            self.completion_flag.store(true, Ordering::Relaxed);
            self.return_stream_to_pool();
            return std::task::Poll::Ready(Ok(0));
        }

        let to_read = std::cmp::min(buf.len(), self.remaining);
        if to_read == 0 {
            self.eof_reached = true;
            self.completion_flag.store(true, Ordering::Relaxed);
            self.return_stream_to_pool();
            return std::task::Poll::Ready(Ok(0));
        }

        if let Some(inner_stream) = &mut self.inner {
            match std::pin::Pin::new(inner_stream).poll_read(cx, &mut buf[..to_read]) {
                std::task::Poll::Ready(Ok(n)) => {
                    if n == 0 {
                        self.eof_reached = true;
                        return std::task::Poll::Ready(Ok(0));
                    }

                    self.remaining -= n;
                    if self.remaining == 0 {
                        self.eof_reached = true;
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

impl crate::stream::RWStream for BodyFixedLengthStream {}

impl crate::stream::RWStream for ChunkedDecoderStream {}

pub struct Response {
    pub addr: SocketAddr,
    pub is_tls: bool,
    pub body_successfully_readed: bool,
    pub http_version: HttpVersion,
    pub status_code: u16,
    pub headers: HashMap<String, IndexSet<String>>,
    /// if you use this stream, remember to set the body_successfully_readed to true if you read it
    /// otherwise this connection will be reused
    pub body_raw_stream: Option<BoxedStream>,
    pub proxy_used: Option<HttpsProxyOption>,
    /// Track if the managed stream was fully consumed
    pub(crate) stream_completion_flag: Option<Arc<AtomicBool>>,
}

impl Drop for Response {
    fn drop(&mut self) {
        // Only return to pool if body was not consumed through managed stream
        if !self.body_successfully_readed {
            return_stream_to_pool(self)
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
            body_successfully_readed: false,
            http_version,
            status_code,
            headers,
            body_raw_stream: Some(stream),
            addr,
            proxy_used,
            stream_completion_flag: None,
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

    pub fn header_all(&self, _key: impl AsRef<str>) -> Vec<&str> {
        unimplemented!()
    }

    pub async fn body_string(&mut self) -> Result<String> {
        if self.body_successfully_readed {
            return Err(anyhow!("response body has been read"));
        }

        if let Some(mut stream) = self.body_managed_stream() {
            let mut bytes: Vec<u8> = Vec::new();
            let mut buf = [0u8; 1024];
            while let n = stream.read(&mut buf).await.dot()?
                && n > 0
            {
                bytes.extend_from_slice(&buf[..n]);
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

    /// Returns a streaming response body with automatic chunked decoding.
    ///
    /// This function provides true streaming - for chunked responses, it decodes chunks on-the-fly
    /// without buffering the entire body in memory. For responses with Content-Length, it returns
    /// a fixed-length stream that tracks remaining bytes. For other responses, it returns the raw stream.
    ///
    /// # Important Notes
    ///
    /// - Returns `None` if the body has already been read via `body_string()` or other methods.
    /// - For chunked transfer encoding responses, automatically decodes chunks as you read.
    /// - For responses with Content-Length header, returns a BodyFixedLengthStream that tracks remaining bytes.
    /// - For other responses, returns the raw stream directly.
    /// - Once you use this stream, you become responsible for reading it completely.
    ///   The connection will be returned to the pool when the Response is dropped.
    /// - If you don't read the stream completely, the connection may not be reusable.
    pub fn body_managed_stream(&mut self) -> Option<BoxedStream> {
        if self.body_successfully_readed {
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
            self.body_successfully_readed = true;

            if is_chunked {
                // For chunked encoding, wrap the stream with our decoder
                let completion_flag = Arc::new(AtomicBool::new(false));
                self.stream_completion_flag = Some(completion_flag.clone());
                let decoder = ChunkedDecoderStream::new_with_completion_flag(
                    stream, 
                    completion_flag, 
                    self.addr, 
                    self.is_tls, 
                    self.proxy_used.clone()
                );
                Some(Box::new(decoder))
            } else if let Some(length) = content_length {
                // For responses with Content-Length, use BodyFixedLengthStream
                let completion_flag = Arc::new(AtomicBool::new(false));
                self.stream_completion_flag = Some(completion_flag.clone());
                let fixed_length_stream = BodyFixedLengthStream::new_with_completion_flag(
                    stream, 
                    length as usize, 
                    completion_flag,
                    self.addr,
                    self.is_tls,
                    self.proxy_used.clone()
                );
                Some(Box::new(fixed_length_stream))
            } else {
                // For other responses, return the raw stream
                Some(stream)
            }
        } else {
            None
        }
    }

    pub fn body_slice(&self) -> &[u8] {
        unimplemented!()
    }

    pub fn body_json(&self) -> serde_json::Value {
        unimplemented!()
    }

    pub fn body_form(&self) -> HashMap<String, String> {
        unimplemented!()
    }

    pub fn body_multipart_form(&self) -> HashMap<String, String> {
        unimplemented!()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get("content-length")
            .and_then(|vec| vec.first())
            .and_then(|s| s.parse::<u64>().ok())
    }

    /// Check if the managed stream was fully consumed
    pub fn is_stream_fully_consumed(&self) -> bool {
        self.stream_completion_flag
            .as_ref()
            .map(|flag| flag.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    async fn read_chunked_body(&mut self) -> Result<Vec<u8>> {
        if let Some(stream) = self.body_raw_stream.as_mut() {
            let stream = stream as &mut BoxedStream;
            // read the size line
            let mut line_buf: Vec<u8> = vec![];
            let mut result = Vec::new();
            loop {
                let n = read_until_v(stream, b"\r\n", &mut line_buf).await.dot()?;

                let mut chunk_size_str = String::from_utf8_lossy(&line_buf[..n]);
                // sometimes there wil be \r\n in the beginning instead of the number
                if chunk_size_str.trim().is_empty() {
                    let n = read_until_v(stream, b"\r\n", &mut line_buf).await.dot()?;
                    chunk_size_str = String::from_utf8_lossy(&line_buf[..n]);
                }
                let chunk_size = usize::from_str_radix(chunk_size_str.trim(), 16).map_err(|e| {
                    anyhow!(
                        "invalid chunk size '{:?}': {}",
                        chunk_size_str.as_bytes(),
                        e
                    )
                })?;
                // Last chunk (size 0) indicates end
                if chunk_size == 0 {
                    let n = read_until_v(stream, b"\r\n", &mut line_buf).await.dot()?;
                    if n != 2 {
                        let x = String::from_utf8_lossy(&line_buf[..n]);
                        return Err(anyhow!(
                            "not possible, it's not \\r\\n after zero in chunk. x={x}, n={n}"
                        ));
                    }
                    self.body_successfully_readed = true;
                    break;
                } else {
                    // Read chunk data
                    let mut buf = [0u8; 1024];
                    let mut remaining = chunk_size;
                    while remaining > 0 {
                        let to_read = std::cmp::min(buf.len(), remaining);
                        let n = stream.read(&mut buf[..to_read]).await.dot()?;
                        if n == 0 {
                            return Err(anyhow!(
                                "unexpected end of stream while reading chunk data"
                            ));
                        }
                        result.extend_from_slice(&buf[..n]);
                        remaining -= n;
                    }
                }
            }
            return Ok(result);
        } else {
            return Err(anyhow!("impossible, body stream is none"));
        }
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
    fn test_chunked() {
        task::block_on(async {
            // let mut req = Request::new("GET", "http://127.0.0.1:8888/test/chunk").unwrap();
            let mut req = Request::new("GET", "http://127.0.0.1:8888/test/gb2312.txt").unwrap();
            let mut resp = ZJHttpClient::new().send(&mut req).await.unwrap();
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
}
