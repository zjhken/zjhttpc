use anyhow_ext::{Context, Result, anyhow};
use async_std::io::ReadExt;
use encoding_rs::GBK;
use hashbrown::HashMap;
use indexmap::IndexSet;
use std::{net::SocketAddr, vec};

use tracing::{error, info, trace};

use crate::{
    client::{read_until_v, return_stream_to_pool},
    error::ZjhttpcError,
    misc::HttpVersion,
    proxy::HttpsProxyOption,
    stream::BoxedStream,
};

/// A streaming chunked decoder that processes chunks on-the-fly without buffering the entire body
pub struct ChunkedDecoderStream {
    inner: BoxedStream,
    state: DecoderState,
    chunk_remaining: usize,
    line_buffer: Vec<u8>,
    eof_reached: bool,
    /// Internal buffer for chunk trailer reading
    trailer_buffer: Vec<u8>,
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
            inner,
            state: DecoderState::ReadingChunkSize,
            chunk_remaining: 0,
            line_buffer: Vec::new(),
            eof_reached: false,
            trailer_buffer: Vec::new(),
        }
    }

    /// Try to read the next chunk size. Returns Ok(true) if a new chunk size was read,
    /// Ok(false) if the final chunk (size 0) was reached, or Err if there was an error.
    async fn read_chunk_size(&mut self) -> Result<bool> {
        self.line_buffer.clear();
        let n = read_until_v(&mut self.inner, b"\r\n", &mut self.line_buffer).await?;

        let mut chunk_size_str = String::from_utf8_lossy(&self.line_buffer[..n]);
        // Sometimes there will be \r\n in the beginning instead of the number
        if chunk_size_str.trim().is_empty() {
            self.line_buffer.clear();
            let n = read_until_v(&mut self.inner, b"\r\n", &mut self.line_buffer).await?;
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
            let n = read_until_v(&mut self.inner, b"\r\n", &mut self.line_buffer).await?;
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
        self.trailer_buffer.clear();
        let n = read_until_v(&mut self.inner, b"\r\n", &mut self.trailer_buffer).await?;
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

                    match std::pin::Pin::new(&mut self.inner).poll_read(cx, &mut temp_buf) {
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

impl crate::stream::RWStream for ChunkedDecoderStream {}

pub struct Response {
    pub addr: SocketAddr,
    pub is_tls: bool,
    pub body_successfully_readed: bool,
    pub http_version: HttpVersion,
    pub status_code: u16,
    pub headers: HashMap<String, IndexSet<String>>,
    /// if you use this stream, remember to set the body_readed to true if you read it
    /// otherwise this connection will be reused
    pub body_stream: Option<BoxedStream>,
    pub proxy_used: Option<HttpsProxyOption>,
}

impl Drop for Response {
    fn drop(&mut self) {
        return_stream_to_pool(self)
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
            body_stream: Some(stream),
            addr,
            proxy_used,
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
        let bytes = match self.content_length() {
            Some(len) => {
                info!(len);
                if len == 0 {
                    return Ok(String::new());
                } else {
                    let mut v = vec![];
                    let stream = self
                        .body_stream
                        .as_mut()
                        .ok_or_else(|| anyhow!("impossible, body stream is none"))
                        .dot()?;
                    let mut remaining = len as usize;
                    let mut buf = [0u8; 1024];
                    while remaining > 0 {
                        let to_read = std::cmp::min(buf.len(), remaining);
                        let n = stream.read(&mut buf[..to_read]).await.dot()?;
                        if n == 0 {
                            info!("stream ended");
                            break;
                        }
                        v.extend_from_slice(&buf[..n]);
                        remaining -= n;
                    }
                    self.body_successfully_readed = true;
                    v
                }
            }
            None => {
                let mut decoded_stream = self.body_decoded_stream();
                let stream = if let Some(set) = self.headers.get("transfer-encoding") {
                    if set.contains("chunked") {
                        decoded_stream.as_mut()
                    } else {
                        self.body_stream.as_mut()
                    }
                } else {
                    self.body_stream.as_mut()
                };

                if let Some(stream) = stream {
                    let mut v = vec![];
                    let mut buf = [0u8; 1024 * 8];
                    loop {
                        let n = stream.read(&mut buf[..]).await.dot()?;
                        if n == 0 {
                            trace!("stream ended");
                            break;
                        }
                        v.extend_from_slice(&buf[..n]);
                    }
                    self.body_successfully_readed = true;
                    v
                } else {
                    Vec::new()
                }
            }
        };
        if let Some(x) = self.headers.get("content-type")
            && x.last().map(|x|x.to_lowercase().contains("charset=gbk")).unwrap_or(false)
        {
            let (cow, _encoding, had_errors) = GBK.decode(&bytes.as_slice());
            if had_errors {
                error!("GBK decode with errors");
            }
            return Ok(cow.to_string());
        } else {
            return Ok(String::from_utf8_lossy(&bytes).to_string());
        }
    }

    /// Returns a streaming response body with automatic chunked decoding.
    ///
    /// This function provides true streaming - for chunked responses, it decodes chunks on-the-fly
    /// without buffering the entire body in memory. For non-chunked responses, it returns the raw stream.
    ///
    /// # Important Notes
    ///
    /// - Returns `None` if the body has already been read via `body_string()` or other methods.
    /// - For chunked transfer encoding responses, automatically decodes chunks as you read.
    /// - For non-chunked responses, returns the raw stream directly.
    /// - Once you use this stream, you become responsible for reading it completely.
    ///   The connection will be returned to the pool when the Response is dropped.
    /// - If you don't read the stream completely, the connection may not be reusable.
    pub fn body_decoded_stream(&mut self) -> Option<BoxedStream> {
        if self.body_successfully_readed {
            return None;
        }

        // Check if this is chunked encoding
        let is_chunked = self
            .headers
            .get("transfer-encoding")
            .map(|set| set.iter().any(|v| v.contains("chunked")))
            .unwrap_or(false);

        if let Some(stream) = self.body_stream.take() {
            self.body_successfully_readed = true;

            if is_chunked {
                // For chunked encoding, wrap the stream with our decoder
                let decoder = ChunkedDecoderStream::new(stream);
                Some(Box::new(decoder))
            } else {
                // For non-chunked responses, return the raw stream
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

    async fn read_chunked_body(&mut self) -> Result<Vec<u8>> {
        if let Some(stream) = self.body_stream.as_mut() {
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
            info!(s);
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
    fn test_gb2312_decoding() {
        let bytes = include_bytes!("/Users/bluewater/codes/stock-noti/dev/gb2312.txt");
        let (a, b, c) = encoding_rs::GBK.decode(bytes);
        println!("{}", a.to_string());
    }
}
