use thiserror::Error;
use anyhow_ext::Result;

type ZjhttpCResult<T> = Result<T, ZjhttpcError>;

#[derive(Debug, Error)]
pub enum ZjhttpcError {
    #[error("failed to parse the URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("invalid/unsupport HTTP version in response:{0}")]
    InvalidHttpResponseVersion(String),
    #[error("invalid HTTP status code in response:{0}")]
    InvalidHttpResponseStatusCode(String),
    #[error("the response body has been read")]
    BodyHasBeenRead,
}
