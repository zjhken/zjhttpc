mod error;
use std::time::Duration;

use error::ZjhttpcError;
use hashbrown::HashMap;
use url::Url;

pub struct HttpClient {
    // connection_pool: unimplemented!(),
    pub global_total_timeout: Duration,
    pub global_receive_first_byte_timeout: Duration,
}

impl HttpClient {
    #[must_use]
    pub fn new() -> HttpClient {
        HttpClient {
            global_total_timeout: Duration::from_secs(300),
            global_receive_first_byte_timeout: Duration::from_secs(30),
        }
    }

    pub fn send(&self, request: impl AsRef<Request>) -> String {
        // Make a request to the URL and return the response
        "Response from the server".to_string()
    }
}

pub struct Request {
    method: Method,
    url: Url,
    headers: HashMap<String, Vec<String>>,
}

impl Request {
    #[must_use]
    pub fn new(url: impl AsRef<str>) -> Result<Self, ZjhttpcError> {
        let url = url.as_ref().parse()?;
        Ok(Request {
            method: Method::get(),
            url,
            headers: HashMap::new(),
        })
    }

    pub fn method(&mut self, method: Method) -> &mut Self {
        self.method = method;
        self
    }

    pub fn add_header(&mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> &mut Self {
        // Add a header to the request
        self
    }

    pub fn set_header(&mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> &mut Self {
        // Set a header to the request
        self
    }

    pub fn set_headers(&mut self, headers: HashMap<String, Vec<String>>) -> &mut Self {
        self
    }

    pub fn header_one(&self, key: impl AsRef<str>) -> Option<&String> {
        unimplemented!()
    }

    pub fn header_all(&self, key: impl AsRef<str>) -> Vec<&String> {
        unimplemented!()
    }

    pub fn put_expect_100_continue(&mut self, expect: bool) -> &mut Self {
        // Set the expect 100 continue header
        self
    }

    pub fn basic_auth(
        &mut self,
        username: impl AsRef<str>,
        password: impl AsRef<str>,
    ) -> &mut Self {
        // Set the basic auth header
        self
    }

    pub fn body_string(&mut self, body: impl AsRef<str>) -> &mut Self {
        // Set the body of the request

        self
    }

    pub fn body_stream(&mut self, body: impl async_std::io::Read) -> &mut Self {
        // Set the body of the request
        self
    }

    pub fn body_slice(&mut self, body: impl AsRef<[u8]>) -> &mut Self {
        // Set the body of the request
        self
    }

    pub fn body_form(&mut self, form: HashMap<String, String>) -> &mut Self {
        // Set the body of the request
        self
    }

    pub fn body_multipart_form(&mut self, form: HashMap<String, String>) -> &mut Self {
        // Set the body of the request
        self
    }
}

struct Response<'a> {
    http_version: String,
    status_code: u16,
    header_buf: [u8; 8192],
    headers: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> Response<'a> {
    pub fn status_code(&self) -> u16 {
        self.status_code
    }

    pub fn header_one(&self, key: impl AsRef<str>) -> Option<&str> {
        unimplemented!()
    }

    pub fn header_all(&self, key: impl AsRef<str>) -> Vec<&str> {
        unimplemented!()
    }

    pub async fn body_string(&self) -> String {
        // Return the body of the response
        "Response body".to_string()
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
}

pub struct Method {
    dynamic: Option<String>,
    predefined: PredefinedMethod,
}

impl Method {
    fn get() -> Method {
        Method {
            dynamic: None,
            predefined: PredefinedMethod::Get,
        }
    }
}

enum PredefinedMethod {
    Get,
    Put,
    Delete,
    Post,
    Options,
    Head,
}

struct HttpProxyOption {
    host: String,
    port: u16,
    username: String,
    password: String,
}
