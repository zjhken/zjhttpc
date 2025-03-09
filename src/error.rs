use thiserror::Error;

#[derive(Debug, Error)]
pub enum ZjhttpcError {
    #[error("failed to parse the URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}
