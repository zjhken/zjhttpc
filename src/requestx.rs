use hashbrown::HashMap;
use indexmap::IndexSet;
use serde::Serialize;
use url::Url;

use anyhow_ext::{Context, Result};
use async_std::fs::File;
use futures::io::BufReader;
use std::time::Duration;

use crate::{
    error::ZjhttpcError,
    misc::{Body, TrustStorePem},
    proxy::HttpsProxyOption,
};

pub struct Request {
    pub method: &'static str,
    pub url: Url,
    pub headers: HashMap<String, IndexSet<String>>,
    pub expect_continue: bool,
    pub content_type: &'static str,
    pub basic_auth: Option<(String, String)>,
    pub content_length: u64,
    pub header_timeout: Option<Duration>,
    pub connect_timeout: Option<Duration>,
    pub body: Body,
    pub trust_store_pem: Option<TrustStorePem>,
    pub proxy: Option<HttpsProxyOption>,
}

const LIB_VERSION: &str = env!("CARGO_PKG_VERSION");

impl Request {
    #[must_use]
    pub fn new(method: &'static str, url: impl AsRef<str>) -> Result<Self> {
        let url: Url = url.as_ref().parse()?;
        let host = url.host_str().ok_or_else(|| ZjhttpcError::NoHost).dot()?;
        let mut headers = HashMap::new();
        headers.insert("host".to_owned(), IndexSet::from([host.to_owned()]));
        headers.insert("user-agent".to_owned(), IndexSet::from([format!("zjhttpc/{LIB_VERSION} (powered by Jinhui)")]));
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
            connect_timeout: None,
            trust_store_pem: None,
            proxy: None,
        })
    }

    pub fn method(mut self, method: &'static str) -> Self {
        self.method = method;
        self
    }

    pub fn add_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        if let Some(v) = self.headers.get_mut(key.as_ref()) {
            v.insert(value.as_ref().to_owned());
        } else {
            self.headers
                .insert(key.as_ref().to_owned(), IndexSet::from([value.as_ref().to_owned()]));
        }
        self
    }

    pub fn set_header(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        // Set a header to the request
        self.headers
            .insert(key.as_ref().to_owned(), IndexSet::from([value.as_ref().to_owned()]));
        self
    }

    pub fn set_headers(mut self, headers: HashMap<String, IndexSet<String>>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn set_headers_nondup(
        mut self,
        headers: std::collections::HashMap<String, String>,
    ) -> Self {
        let map = headers
            .iter()
            .map(|(k, v)| (k.to_owned(), IndexSet::from([v.to_owned()])))
            .collect::<HashMap<_, _>>();
        self.headers.extend(map);
        self
    }

    pub fn set_queries_serde(mut self, queries: &impl Serialize) -> Result<Self> {
        let s = serde_qs::to_string(queries).dot()?;
        self.url.set_query(Some(s.as_str()));
        Ok(self)
    }

    pub fn add_query(mut self, key: &str, value: &str) -> Self {
        {
            let mut query_pairs = self.url.query_pairs_mut();
            query_pairs.append_pair(key, value);
        }
        self
    }

    pub fn header_one(&self, key: impl AsRef<str>) -> Option<&String> {
        self.headers.get(key.as_ref()).and_then(|set| set.first())
    }

    pub fn header_all(&self, key: impl AsRef<str>) -> Option<&IndexSet<String>> {
        self.headers.get(key.as_ref())
    }

    pub fn put_expect_continue(mut self, _expect: bool) -> Self {
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

    pub fn body_slice(mut self, body: impl AsRef<[u8]>) -> Self {
        // Set the body of the request
        let bytes = body.as_ref();
        self.content_length = bytes.len() as u64;
        self.body = Body::Bytes(bytes.to_vec());
        self
    }

    pub fn body_form(self, _form: HashMap<String, String>) -> Self {
        // Set the body of the request
        unimplemented!();
    }

    pub fn body_multipart_form(self, _form: HashMap<String, String>) -> Self {
        // Set the body of the request
        unimplemented!();
    }

    pub fn set_header_timeout(mut self, dur: Duration) -> Self {
        self.header_timeout = Some(dur);
        self
    }

    pub fn set_proxy(mut self, proxy: HttpsProxyOption) -> Self {
        self.proxy = Some(proxy);
        self
    }

    pub fn set_proxy_from_url(mut self, proxy_url: impl AsRef<str>) -> Result<Self> {
        let proxy = HttpsProxyOption::new(proxy_url)?;
        self.proxy = Some(proxy);
        Ok(self)
    }

    pub fn set_connect_timeout(mut self, dur: Duration) -> Self {
        self.connect_timeout = Some(dur);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn test_request_proxy_configuration() {
        let mut request = Request::new("GET", "http://example.com").unwrap();
        assert!(request.proxy.is_none());

        let proxy = crate::proxy::HttpsProxyOption::new("http://proxy.example.com:8080").unwrap();
        request = request.set_proxy(proxy.clone());
        assert!(request.proxy.is_some());
        assert_eq!(request.proxy.unwrap().url.host_str().unwrap(), "proxy.example.com");
    }

    #[test]
    fn test_request_proxy_from_url() {
        let result = Request::new("GET", "http://example.com").unwrap()
            .set_proxy_from_url("http://proxy.example.com:8080");
        assert!(result.is_ok());
        let request = result.unwrap();
        assert!(request.proxy.is_some());
        assert_eq!(request.proxy.unwrap().url.host_str().unwrap(), "proxy.example.com");
    }

    #[test]
    fn test_request_invalid_proxy_url() {
        let result = Request::new("GET", "http://example.com").unwrap()
            .set_proxy_from_url("invalid-url");
        assert!(result.is_err());
    }

    #[test]
    fn test_request_connect_timeout() {
        let request = Request::new("GET", "http://example.com").unwrap()
            .set_connect_timeout(Duration::from_secs(5));
        assert_eq!(request.connect_timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn test_request_connect_timeout_default() {
        let request = Request::new("GET", "http://example.com").unwrap();
        assert_eq!(request.connect_timeout, None);
    }

    #[test]
    fn test_add_query_to_url_without_existing_query() {
        let request = Request::new("GET", "http://example.com")
            .unwrap()
            .add_query("param1", "value1")
            .add_query("param2", "value2");
        
        assert_eq!(request.url.query(), Some("param1=value1&param2=value2"));
    }

    #[test]
    fn test_add_query_to_url_with_existing_query() {
        let request = Request::new("GET", "http://example.com?existing=test")
            .unwrap()
            .add_query("param1", "value1");
        
        assert_eq!(request.url.query(), Some("existing=test&param1=value1"));
    }

    #[test]
    fn test_add_query_with_special_characters() {
        let request = Request::new("GET", "http://example.com")
            .unwrap()
            .add_query("query", "hello world")
            .add_query("symbol", "@#$%");
        
        let query = request.url.query().unwrap();
        assert!(query.contains("query=hello+world"));
        assert!(query.contains("symbol=%40%23%24%25"));
    }

    #[test]
    fn test_add_query_with_empty_values() {
        let request = Request::new("GET", "http://example.com")
            .unwrap()
            .add_query("empty", "")
            .add_query("param", "value");
        
        assert_eq!(request.url.query(), Some("empty=&param=value"));
    }

    #[test]
    fn test_add_query_with_duplicate_keys() {
        let request = Request::new("GET", "http://example.com")
            .unwrap()
            .add_query("key", "value1")
            .add_query("key", "value2");
        
        let query = request.url.query().unwrap();
        assert!(query.contains("key=value1"));
        assert!(query.contains("key=value2"));
        assert_eq!(query, "key=value1&key=value2");
    }

    #[test]
    fn test_add_query_to_https_url() {
        let request = Request::new("GET", "https://api.example.com/endpoint")
            .unwrap()
            .add_query("api_key", "secret123")
            .add_query("format", "json");
        
        assert_eq!(request.url.query(), Some("api_key=secret123&format=json"));
        assert_eq!(request.url.scheme(), "https");
        assert_eq!(request.url.path(), "/endpoint");
    }

    #[test]
    fn test_add_query_with_path_and_fragment() {
        let request = Request::new("GET", "http://example.com/path/to/resource#section")
            .unwrap()
            .add_query("filter", "all");
        
        assert_eq!(request.url.query(), Some("filter=all"));
        assert_eq!(request.url.path(), "/path/to/resource");
        assert_eq!(request.url.fragment(), Some("section"));
    }

    #[test]
    fn test_add_query_unicode_characters() {
        let request = Request::new("GET", "http://example.com")
            .unwrap()
            .add_query("emoji", "🚀")
            .add_query("chinese", "你好");
        
        let query = request.url.query().unwrap();
        assert!(query.contains("emoji=%F0%9F%9A%80"));
        assert!(query.contains("chinese=%E4%BD%A0%E5%A5%BD"));
    }

    #[test]
    fn test_add_query_chainable_api() {
        let request = Request::new("GET", "http://example.com")
            .unwrap()
            .add_query("a", "1")
            .add_query("b", "2")
            .add_header("Accept", "application/json")
            .add_query("c", "3");
        
        assert_eq!(request.url.query(), Some("a=1&b=2&c=3"));
        assert!(request.headers.contains_key("Accept"));
        assert_eq!(request.headers.get("Accept").unwrap().first().unwrap(), "application/json");
    }
}
