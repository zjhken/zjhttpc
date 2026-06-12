use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Error type for zjhttpc operations.
///
/// All public API functions return `Result<T, ZjhttpcError>`.
/// Callers can match on specific variants to handle different error categories.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum ZjhttpcError {
    // URL / Request validation
    #[error("URL parse error: {0}")]
    InvalidUrl(#[from] url::ParseError),

    #[error("no host in URL")]
    NoHost,

    #[error("URL must have a valid port")]
    NoPort,

    #[error("unsupported scheme: {0}")]
    UnsupportedScheme(String),

    // DNS
    #[error("DNS resolution failed: {0}")]
    Dns(String),

    // Connection
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("connection timeout after {0:?}")]
    ConnectionTimeout(Duration),

    // TLS / Certificate
    #[error("TLS error: {0}")]
    Tls(String),

    #[error("certificate error: {0}")]
    Certificate(String),

    // Proxy
    #[error("proxy error: {0}")]
    Proxy(String),

    // Timeout
    #[error("send header timeout after {0:?}")]
    SendHeaderTimeout(Duration),

    #[error("read header timeout after {0:?}")]
    ReadHeaderTimeout(Duration),

    #[error("read body timeout after {0:?}")]
    ReadBodyTimeout(Duration),

    // Response parsing
    #[error("invalid HTTP response: {0}")]
    InvalidResponse(String),

    #[error("response headers exceeded limit ({actual} > {max})")]
    ResponseTooLarge { actual: usize, max: usize },

    #[error("unexpected EOF: {0}")]
    UnexpectedEof(String),

    // Body
    #[error("response body has already been read")]
    BodyAlreadyRead,

    #[error("JSON parsing failed: {message}")]
    JsonParsing { message: String, preview: String },

    // Query serialization
    #[error("query serialization error: {0}")]
    QuerySerialize(String),

    // IO
    #[error("{0}")]
    Io(Arc<std::io::Error>),
}

impl From<std::io::Error> for ZjhttpcError {
    fn from(e: std::io::Error) -> Self {
        ZjhttpcError::Io(Arc::new(e))
    }
}

impl From<serde_qs::Error> for ZjhttpcError {
    fn from(e: serde_qs::Error) -> Self {
        ZjhttpcError::QuerySerialize(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ZjhttpcError>;
