
use hashbrown::HashMap;
use url::Url;

use anyhow_ext::{Context, Result};
use async_std::fs::File;
use futures::io::BufReader;
use std::time::Duration;

use crate::{error::ZjhttpcError, misc::{Body, TrustStorePem}};

pub struct Request {
    pub method: &'static str,
    pub url: Url,
    pub headers: HashMap<String, Vec<String>>,
    pub expect_continue: bool,
    pub content_type: &'static str,
    pub basic_auth: Option<(String, String)>,
    pub content_length: u64,
    pub header_timeout: Option<Duration>,
    pub body: Body,
    pub trust_store_pem: Option<TrustStorePem>,
}

impl Request {
    #[must_use]
    pub fn new(url: impl AsRef<str>) -> Result<Self, ZjhttpcError> {
        let url = url.as_ref().parse()?;
        Ok(Request {
            method: "GET",
            url,
            headers: HashMap::new(),
            expect_continue: false,
            content_type: "application/octet-stream",
            basic_auth: None,
            body: Body::None,
            content_length: 0,
            header_timeout: None,
            trust_store_pem: None,
        })
    }

    pub fn method(&mut self, method: &'static str) -> &mut Self {
        self.method = method;
        self
    }

    pub fn add_header(&mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> &mut Self {
        // Add a header to the request
        unimplemented!();
        self
    }

    pub fn set_header(&mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> &mut Self {
        // Set a header to the request
        self.headers
            .insert(key.as_ref().to_owned(), vec![value.as_ref().to_owned()]);
        self
    }

    pub fn set_headers(&mut self, headers: HashMap<String, Vec<String>>) -> &mut Self {
        self.headers.extend(headers);
        self
    }

    pub fn header_one(&self, key: impl AsRef<str>) -> Option<&String> {
        unimplemented!()
    }

    pub fn header_all(&self, key: impl AsRef<str>) -> Vec<&String> {
        unimplemented!()
    }

    pub fn put_expect_continue(&mut self, expect: bool) -> &mut Self {
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

    pub fn set_basic_auth(
        &mut self,
        username: impl AsRef<str>,
        password: impl AsRef<str>,
    ) -> &mut Self {
        // Set the basic auth header
        self.basic_auth = Some((username.as_ref().to_owned(), password.as_ref().to_owned()));
        self
    }

    pub fn set_body_string(&mut self, body: impl AsRef<str>) -> &mut Self {
        // Set the body of the request
        self.content_length = body.as_ref().len() as u64;
        self.body = Body::Str(body.as_ref().to_owned());
        self
    }

    pub fn set_body_stream<R>(&mut self, body: R, length: u64) -> &mut Self 
    where 
        R:async_std::io::Read + Unpin + Send + Sync + 'static,
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

    pub fn body_slice(&mut self, body: impl AsRef<[u8]>) -> &mut Self {
        // Set the body of the request
        unimplemented!();
        self
    }

    pub fn body_form(&mut self, form: HashMap<String, String>) -> &mut Self {
        // Set the body of the request
        unimplemented!();
        self
    }

    pub fn body_multipart_form(&mut self, form: HashMap<String, String>) -> &mut Self {
        // Set the body of the request
        unimplemented!();
        self
    }

    pub fn set_header_timeout(mut self, dur: Duration) -> Self {
        self.header_timeout = Some(dur);
        self
    }
}
