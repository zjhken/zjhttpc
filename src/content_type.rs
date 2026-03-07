//! Common HTTP content-type definitions
//!
//! This module provides commonly used MIME media type constants for HTTP content-type headers.
//! Usage:
//! ```ignore
//! use zjhttpc::content_type;
//!
//! let request = Request::new("GET", "http://example.com")?
//!     .set_content_type(content_type::APPLICATION_JSON);
//! ```

/// JSON content type
pub const APPLICATION_JSON: &str = "application/json";

/// Plain text content type
pub const TEXT_PLAIN: &str = "text/plain";

/// HTML content type
pub const TEXT_HTML: &str = "text/html";

/// CSS content type
pub const TEXT_CSS: &str = "text/css";

/// JavaScript content type
pub const TEXT_JAVASCRIPT: &str = "text/javascript";

/// XML content type
pub const APPLICATION_XML: &str = "application/xml";

/// Text XML content type
pub const TEXT_XML: &str = "text/xml";

/// URL-encoded form data content type
pub const APPLICATION_X_WWW_FORM_URLENCODED: &str = "application/x-www-form-urlencoded";

/// Multipart form data content type
pub const MULTIPART_FORM_DATA: &str = "multipart/form-data";

/// Octet-stream (binary file) content type
pub const APPLICATION_OCTET_STREAM: &str = "application/octet-stream";

/// PDF content type
pub const APPLICATION_PDF: &str = "application/pdf";

/// ZIP archive content type
pub const APPLICATION_ZIP: &str = "application/zip";

/// GZIP content type
pub const APPLICATION_GZIP: &str = "application/gzip";

/// JSON Web Token content type
pub const APPLICATION_JWT: &str = "application/jwt";

/// PNG image content type
pub const IMAGE_PNG: &str = "image/png";

/// JPEG image content type
pub const IMAGE_JPEG: &str = "image/jpeg";

/// GIF image content type
pub const IMAGE_GIF: &str = "image/gif";

/// WebP image content type
pub const IMAGE_WEBP: &str = "image/webp";

/// SVG image content type
pub const IMAGE_SVG_XML: &str = "image/svg+xml";

/// ICO image content type
pub const IMAGE_ICON: &str = "image/x-icon";

/// MP4 video content type
pub const VIDEO_MP4: &str = "video/mp4";

/// MPEG video content type
pub const VIDEO_MPEG: &str = "video/mpeg";

/// WebM video content type
pub const VIDEO_WEBM: &str = "video/webm";

/// MP3 audio content type
pub const AUDIO_MP3: &str = "audio/mpeg";

/// MP4 audio content type
pub const AUDIO_MP4: &str = "audio/mp4";

/// WebM audio content type
pub const AUDIO_WEBM: &str = "audio/webm";

/// WAV audio content type
pub const AUDIO_WAV: &str = "audio/wav";

/// OGG audio content type
pub const AUDIO_OGG: &str = "audio/ogg";

/// Message format (used in Apple Push Notification Service)
pub const APPLICATION_MSGPACK: &str = "application/msgpack";

/// Protocol Buffers content type
pub const APPLICATION_PROTOBUF: &str = "application/protobuf";

/// TOML content type
pub const APPLICATION_TOML: &str = "application/toml";

/// YAML content type
pub const APPLICATION_X_YAML: &str = "application/x-yaml";

/// CSV content type
pub const TEXT_CSV: &str = "text/csv";

/// Markdown content type
pub const TEXT_MARKDOWN: &str = "text/markdown";
