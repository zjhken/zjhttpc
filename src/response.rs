use anyhow_ext::{Context, Result, anyhow};
use async_std::io::ReadExt;
use hashbrown::HashMap;
use std::{net::SocketAddr, vec};

use tracing::info;

use crate::{client::return_stream_to_pool, error::ZjhttpcError, misc::HttpVersion, stream::BoxedStream};

pub struct Response {
    pub addr: SocketAddr,
    pub is_tls: bool,
    pub body_readed: bool,
    pub http_version: HttpVersion,
    pub status_code: u16,
    pub headers: HashMap<String, Vec<String>>,
    /// if you use this stream, remember to set the body_readed to true if you read it
    /// otherwise this connection will be reused
    pub body_stream: Option<BoxedStream>,
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
    ) -> Result<Self, ZjhttpcError> {
        let http_version = match http_version {
            "1.1" => HttpVersion::V1_1,
            "1.0" => HttpVersion::V1_0,
            others => return Err(ZjhttpcError::InvalidHttpResponseVersion(others.to_string())),
        };
        let status_code: u16 = status_code
            .parse()
            .map_err(|e| ZjhttpcError::InvalidHttpResponseStatusCode(status_code.to_string()))?;
        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        for (key, value) in headers_vec {
            match headers.get_mut(&key) {
                Some(vec) => vec.push(value),
                None => {
                    headers.insert(key, vec![value]);
                }
            }
        }
        let mut resp = Response {
            is_tls,
            body_readed: false,
            http_version,
            status_code,
            headers,
            body_stream: Some(stream),
            addr,
        };
        if resp.content_length() == Some(0) {
            resp.body_readed = true;
        }
        return Ok(resp);
    }
    pub fn status_code(&self) -> u16 {
        self.status_code
    }

    pub fn is_success(&self) -> bool {
        (200u16..300u16).contains(&self.status_code)
    }

    pub fn header_one(&self, key: impl AsRef<str>) -> Option<&str> {
        unimplemented!()
    }

    pub fn header_all(&self, key: impl AsRef<str>) -> Vec<&str> {
        unimplemented!()
    }

    pub async fn body_string(&mut self) -> Result<String> {
        if self.body_readed {
            return Err(anyhow!("response body has been read"));
        }
        match self.content_length() {
            Some(len) => {
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
                    self.body_readed = true;
                    return String::from_utf8(v).dot();
                }
            },
            None => {
                // TODO: handle chunk download
                return Err(anyhow!("chunk download is not supported yet"))
            }
        }
    }

    // pub fn body_stream(&self) -> impl async_std::io::Read {
    //     unimplemented!()
    // }

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
}
