use hashbrown::HashMap;
use serde::Serialize;
use url::Url;

use anyhow_ext::{Context, Result};
use async_std::fs::File;
use futures::io::BufReader;
use std::time::Duration;

use crate::{
    error::ZjhttpcError,
    misc::{Body, TrustStorePem},
};

pub struct Request {
    pub method: &'static str,
    pub url: Url,
    // TODO: change vec to hashSet
    pub headers: HashMap<String, Vec<String>>,
    pub expect_continue: bool,
    pub content_type: &'static str,
    pub basic_auth: Option<(String, String)>,
    pub content_length: u64,
    pub header_timeout: Option<Duration>,
    pub body: Body,
    pub trust_store_pem: Option<TrustStorePem>,
}

const LIB_VERSION: &str = env!("CARGO_PKG_VERSION");

impl Request {
    #[must_use]
    pub fn new(method: &'static str, url: impl AsRef<str>) -> Result<Self> {
        let url: Url = url.as_ref().parse()?;
        let host = url.host_str().ok_or_else(|| ZjhttpcError::NoHost).dot()?;
        let mut headers = HashMap::new();
        headers.insert("host".to_owned(), vec![host.to_owned()]);
        headers.insert("user-agent".to_owned(), vec![format!("zjhttpc/{LIB_VERSION} (powered by Jinhui)")]);
        Ok(Request {
            method,
            url,
            headers,
            expect_continue: false,
            content_type: "application/octet-stream",
            basic_auth: None,
            body: Body::None,
            content_length: 0,
            header_timeout: None,
            trust_store_pem: None,
        })
    }

    pub fn method(mut self, method: &'static str) -> Self {
        self.method = method;
        self
    }

    pub fn add_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        if let Some(v) = self.headers.get_mut(key.as_ref()) {
            v.push(value.as_ref().to_owned());
        } else {
            self.headers
                .insert(key.as_ref().to_owned(), vec![value.as_ref().to_owned()]);
        }
        self
    }

    pub fn set_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        // Set a header to the request
        self.headers
            .insert(key.as_ref().to_owned(), vec![value.as_ref().to_owned()]);
        self
    }

    pub fn set_headers(mut self, headers: HashMap<String, Vec<String>>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn set_headers_nondup(
        mut self,
        headers: std::collections::HashMap<String, String>,
    ) -> Self {
        let map = headers
            .iter()
            .map(|(k, v)| (k.to_owned(), vec![v.to_owned()]))
            .collect::<HashMap<_, _>>();
        self.headers.extend(map);
        self
    }

    pub fn set_queries_serde(mut self, queries: &impl Serialize) -> Result<Self> {
        let s = serde_qs::to_string(queries).dot()?;
        self.url.set_query(Some(s.as_str()));
        Ok(self)
    }

    pub fn header_one(&self, key: impl AsRef<str>) -> Option<&String> {
        unimplemented!()
    }

    pub fn header_all(&self, key: impl AsRef<str>) -> Vec<&String> {
        unimplemented!()
    }

    pub fn put_expect_continue(mut self, expect: bool) -> Self {
        self.expect_continue = true;
        self
    }

    pub fn set_content_type(mut self, content_type: &'static str) -> Self {
        self.content_type = content_type;
        self
    }

    pub fn set_content_length(mut self, len: u64) -> Self {
        self.content_length = len;
        return self;
    }

    pub fn set_basic_auth(mut self, username: impl AsRef<str>, password: impl AsRef<str>) -> Self {
        // Set the basic auth header
        self.basic_auth = Some((username.as_ref().to_owned(), password.as_ref().to_owned()));
        self
    }

    pub fn set_body_string(mut self, body: impl AsRef<str>) -> Self {
        // Set the body of the request
        self.content_length = body.as_ref().len() as u64;
        self.body = Body::Str(body.as_ref().to_owned());
        self
    }

    pub fn set_body_stream<R>(mut self, body: R, length: u64) -> Self
    where
        R: async_std::io::Read + Unpin + Send + Sync + 'static,
    {
        self.content_length = length;
        self.body = Body::Stream(Box::new(body));
        self
    }

    pub async fn set_body_file(mut self, file_path: impl AsRef<std::path::Path>) -> Result<Self> {
        let p = file_path.as_ref().to_owned();
        let p = async_std::path::PathBuf::from(p);
        let len = p.metadata().await.dot()?.len();
        self.content_length = len;
        let file = File::open(p).await.dot()?;
        let buf_reader = BufReader::new(file);
        self.body = Body::Stream(Box::new(buf_reader));
        Ok(self)
    }

    pub fn body_slice(self, body: impl AsRef<[u8]>) -> Self {
        // Set the body of the request
        unimplemented!();
        self
    }

    pub fn body_form(self, form: HashMap<String, String>) -> Self {
        // Set the body of the request
        unimplemented!();
        self
    }

    pub fn body_multipart_form(self, form: HashMap<String, String>) -> Self {
        // Set the body of the request
        unimplemented!();
        self
    }

    pub fn set_header_timeout(mut self, dur: Duration) -> Self {
        self.header_timeout = Some(dur);
        self
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    #[test]
    fn test_url_parsing() {
        // Test basic URL parsing
        let url = Url::parse("http://example.com/path").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str().unwrap(), "example.com");
        assert_eq!(url.path(), "/path");
        println!("{x:?}", x = url.fragment());

        // Test HTTPS URL
        let url = Url::parse("https://example.com:443/secure").unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.port(), None); // wried

        let url = Url::parse("https://example.com:1443/secure").unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.port(), Some(1443)); // wried

        // Test URL with query parameters
        let url = Url::parse("http://example.com/search?q=test&page=1").unwrap();
        assert_eq!(url.query(), Some("q=test&page=1"));

        // Test URL with basic auth
        let url = Url::parse("http://user:pass@example.com").unwrap();
        assert_eq!(url.username(), "user");
        assert_eq!(url.password(), Some("pass"));

        // Test invalid URL
        assert!(Url::parse("not a url").is_err());
    }

    #[test]
    fn test_url_set_query() {
        let mut url = Url::parse("http://user:pass@example.com").unwrap();
        url.query_pairs_mut().append_pair("a", "b");
        url.query_pairs_mut().append_pair("c", "d");
        // url.set_query(Some("c=d"));
        println!("{x}", x = url.to_string())
    }
}
