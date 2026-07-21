use std::sync::Arc;
use std::time::Duration;
use snafu::Snafu;

/// Error type for zjhttpc operations.
///
/// All public API functions return `Result<T, ZjhttpcError>`.
/// Callers can match on specific variants to handle different error categories.
///
/// Each variant carries an implicit [`snafu::Location`] captured automatically
/// at the construction site (via the `*Snafu` selector or through a `#[track_caller]`
/// `From` impl), so callers can locate the source line via `ErrorCompat` or by
/// formatting the location.
#[derive(Debug, Clone, Snafu)]
#[snafu(visibility(pub))]
#[non_exhaustive]
pub enum ZjhttpcError {
    // URL / Request validation
    #[snafu(display("URL parse error: {source} at {location}"))]
    InvalidUrl {
        #[snafu(source)]
        source: url::ParseError,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("no host in URL at {location}"))]
    NoHost {
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("URL must have a valid port at {location}"))]
    NoPort {
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("unsupported scheme: {scheme} at {location}"))]
    UnsupportedScheme {
        scheme: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // DNS
    #[snafu(display("DNS resolution failed: {message} at {location}"))]
    Dns {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // Connection
    #[snafu(display("connection failed: {message} at {location}"))]
    Connection {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("connection timeout after {duration:?} at {location}"))]
    ConnectionTimeout {
        duration: Duration,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // TLS / Certificate
    #[snafu(display("TLS error: {message} at {location}"))]
    Tls {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("certificate error: {message} at {location}"))]
    Certificate {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // Proxy
    #[snafu(display("proxy error: {message} at {location}"))]
    Proxy {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // Timeout
    #[snafu(display("send header timeout after {duration:?} at {location}"))]
    SendHeaderTimeout {
        duration: Duration,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("read header timeout after {duration:?} at {location}"))]
    ReadHeaderTimeout {
        duration: Duration,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("read body timeout after {duration:?} at {location}"))]
    ReadBodyTimeout {
        duration: Duration,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // Response parsing
    #[snafu(display("invalid HTTP response: {message} at {location}"))]
    InvalidResponse {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("response headers exceeded limit ({actual} > {max}) at {location}"))]
    ResponseTooLarge {
        actual: usize,
        max: usize,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("unexpected EOF: {message} at {location}"))]
    UnexpectedEof {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // Body
    #[snafu(display("response body has already been read at {location}"))]
    BodyAlreadyRead {
        #[snafu(implicit)]
        location: snafu::Location,
    },

    #[snafu(display("JSON parsing failed: {message} at {location}"))]
    JsonParsing {
        message: String,
        preview: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // Query serialization (serde_qs::Error is not Clone, so we keep its display string)
    #[snafu(display("query serialization error: {message} at {location}"))]
    QuerySerialize {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // Multipart
    #[snafu(display("multipart content-length computation failed: {message} at {location}"))]
    MultipartContentLength {
        message: String,
        #[snafu(implicit)]
        location: snafu::Location,
    },

    // IO
    #[snafu(display("{source} at {location}"))]
    Io {
        #[snafu(source(from(std::io::Error, Arc::new)))]
        source: Arc<std::io::Error>,
        #[snafu(implicit)]
        location: snafu::Location,
    },
}

impl ZjhttpcError {
    /// Returns the source code location where this error was constructed, if available.
    pub fn location(&self) -> Option<&snafu::Location> {
        Some(match self {
            ZjhttpcError::InvalidUrl { location, .. }
            | ZjhttpcError::NoHost { location }
            | ZjhttpcError::NoPort { location }
            | ZjhttpcError::UnsupportedScheme { location, .. }
            | ZjhttpcError::Dns { location, .. }
            | ZjhttpcError::Connection { location, .. }
            | ZjhttpcError::ConnectionTimeout { location, .. }
            | ZjhttpcError::Tls { location, .. }
            | ZjhttpcError::Certificate { location, .. }
            | ZjhttpcError::Proxy { location, .. }
            | ZjhttpcError::SendHeaderTimeout { location, .. }
            | ZjhttpcError::ReadHeaderTimeout { location, .. }
            | ZjhttpcError::ReadBodyTimeout { location, .. }
            | ZjhttpcError::InvalidResponse { location, .. }
            | ZjhttpcError::ResponseTooLarge { location, .. }
            | ZjhttpcError::UnexpectedEof { location, .. }
            | ZjhttpcError::BodyAlreadyRead { location }
            | ZjhttpcError::JsonParsing { location, .. }
            | ZjhttpcError::QuerySerialize { location, .. }
            | ZjhttpcError::MultipartContentLength { location, .. }
            | ZjhttpcError::Io { location, .. } => location,
        })
    }
}

#[track_caller]
fn caller_location() -> snafu::Location {
    snafu::Location::default()
}

impl From<std::io::Error> for ZjhttpcError {
    #[track_caller]
    fn from(e: std::io::Error) -> Self {
        ZjhttpcError::Io {
            source: Arc::new(e),
            location: caller_location(),
        }
    }
}

impl From<serde_qs::Error> for ZjhttpcError {
    #[track_caller]
    fn from(e: serde_qs::Error) -> Self {
        ZjhttpcError::QuerySerialize {
            message: e.to_string(),
            location: caller_location(),
        }
    }
}

impl From<url::ParseError> for ZjhttpcError {
    #[track_caller]
    fn from(e: url::ParseError) -> Self {
        ZjhttpcError::InvalidUrl {
            source: e,
            location: caller_location(),
        }
    }
}

pub type Result<T> = std::result::Result<T, ZjhttpcError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snafu_selector_captures_caller_location() {
        let err = DnsSnafu { message: "test".to_string() }.build();
        let loc = err.location().expect("location should be captured");
        assert!(
            loc.file.ends_with("error.rs"),
            "expected file to end with error.rs, got {}",
            loc.file,
        );
        assert!(loc.line > 0);
        assert!(loc.column > 0);
        let s = format!("{err}");
        assert!(
            s.contains("error.rs") && s.contains(':'),
            "display should include location, got: {s}",
        );
    }

    #[test]
    fn from_io_error_captures_caller_location() {
        fn fallible() -> Result<()> {
            // intentional io error
            let _ = std::fs::File::open("/nonexistent-zjhttpc-test-path-xyz")?;
            Ok(())
        }
        let err = fallible().unwrap_err();
        match err {
            ZjhttpcError::Io { location, .. } => {
                assert!(
                    location.file.ends_with("error.rs"),
                    "From<io::Error> should capture caller location via #[track_caller], got {}",
                    location.file,
                );
            }
            other => panic!("expected Io, got {other:?}"),
        }
    }
}
