use anyhow_ext::Result;
use async_std::fs::File;
use std::fmt;
use std::path::PathBuf;


/// Request body types
pub enum Body {
    /// String body
    Str(String),
    /// Stream body (for streaming data)
    Stream(Box<dyn async_std::io::Read + Unpin + Send + Sync>),
    /// Bytes body
    Bytes(Vec<u8>),
    /// Multipart form data
    MultipartForm(BodyMultipartForm),
    /// No body
    None,
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Body::Str(s) => f.debug_tuple("Str").field(&s.len()).finish(),
            Body::Stream(_) => f.debug_tuple("Stream").finish(),
            Body::Bytes(b) => f.debug_tuple("Bytes").field(&b.len()).finish(),
            Body::MultipartForm(form) => f.debug_tuple("MultipartForm").field(&form.fields.len()).finish(),
            Body::None => f.debug_tuple("None").finish(),
        }
    }
}

/// Form data for application/x-www-form-urlencoded
#[derive(Clone, Default)]
pub struct BodyForm {
    fields: Vec<(String, String)>,
}

impl BodyForm {
    /// Create a new empty BodyForm
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a key-value pair to the form
    ///
    /// # Arguments
    /// * `key` - The field name
    /// * `value` - The field value
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::body::BodyForm;
    ///
    /// let form = BodyForm::new()
    ///     .add("username", "alice")
    ///     .add("password", "secret")
    ///     .add("tags", "rust")
    ///     .add("tags", "http");
    /// ```
    #[must_use]
    pub fn add(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.fields.push((
            key.as_ref().to_owned(),
            value.as_ref().to_owned(),
        ));
        self
    }

    /// Serialize the form data to application/x-www-form-urlencoded format
    #[must_use]
    pub fn serialize(&self) -> String {
        self.fields
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    url_encode(key),
                    url_encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    /// Get the number of fields in the form
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Check if the form is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

impl fmt::Debug for BodyForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BodyForm")
            .field("fields", &self.fields)
            .finish()
    }
}

/// URL encode a string (percent encoding)
fn url_encode(s: &str) -> String {
    let mut result = String::new();
    for byte in s.bytes() {
        match byte {
            // Unreserved characters - don't encode
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' |
            b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            // Space becomes +
            b' ' => {
                result.push('+');
            }
            // Everything else is percent-encoded
            b => {
                let high = (b >> 4) & 0x0f;
                let low = b & 0x0f;
                let hex = |n: u8| if n < 10 { b'0' + n } else { b'A' + n - 10 };
                result.push('%');
                result.push(hex(high) as char);
                result.push(hex(low) as char);
            }
        }
    }
    result
}

/// Represents a single field in a multipart form
pub enum MultipartField {
    /// A text field with name and value
    Text(String, String),
    /// A file from a path: (name, path, filename, content_type)
    /// filename and content_type are optional (auto-detected if None)
    FilePath(String, PathBuf, Option<String>, Option<String>),
    /// An already opened file: (name, file, filename, content_type)
    File(String, File, Option<String>, Option<String>),
    /// A generic stream: (name, stream, filename, content_type)
    Stream(
        String,
        Box<dyn async_std::io::Read + Unpin + Send + Sync>,
        Option<String>,
        Option<String>,
    ),
}

/// Multipart form data for multipart/form-data
pub struct BodyMultipartForm {
    pub(crate) fields: Vec<MultipartField>,
    pub(crate) boundary: String,
}

impl BodyMultipartForm {
    /// Create a new multipart form with auto-generated boundary
    #[must_use]
    pub fn new() -> Self {
        Self {
            fields: Vec::new(),
            boundary: generate_boundary(),
        }
    }

    /// Add a text field to the form
    ///
    /// # Arguments
    /// * `name` - Field name
    /// * `value` - Field value
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::body::BodyMultipartForm;
    ///
    /// let form = BodyMultipartForm::new()
    ///     .add("username", "alice")
    ///     .add("bio", "Hello, world!");
    /// ```
    #[must_use]
    pub fn add(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.fields.push(MultipartField::Text(
            name.as_ref().to_owned(),
            value.as_ref().to_owned(),
        ));
        self
    }

    /// Add a file from a path (auto-detect filename and content-type)
    ///
    /// # Arguments
    /// * `name` - Field name
    /// * `path` - Path to the file
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::body::BodyMultipartForm;
    /// use std::path::PathBuf;
    ///
    /// # fn main() -> anyhow::Result<()> {
    /// let form = BodyMultipartForm::new()
    ///     .add("username", "alice")
    ///     .add_file_path("avatar", PathBuf::from("/path/to/avatar.jpg"))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn add_file_path(
        mut self,
        name: impl AsRef<str>,
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let path = PathBuf::from(path.as_ref());
        self.fields.push(MultipartField::FilePath(
            name.as_ref().to_owned(),
            path,
            None,  // auto-detect filename
            None,  // auto-detect content-type
        ));
        Ok(self)
    }

    /// Add a file from a path with custom filename and/or content-type
    ///
    /// # Arguments
    /// * `name` - Field name
    /// * `path` - Path to the file
    /// * `filename` - Custom filename (None to use original filename)
    /// * `content_type` - Custom content-type (None to auto-detect)
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::body::BodyMultipartForm;
    /// use std::path::PathBuf;
    ///
    /// # fn main() -> anyhow::Result<()> {
    /// let form = BodyMultipartForm::new()
    ///     .add_file_path_with_options(
    ///         "avatar",
    ///         PathBuf::from("/path/to/image"),
    ///         Some("profile.jpg"),
    ///         Some("image/jpeg")
    ///     )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn add_file_path_with_options(
        mut self,
        name: impl AsRef<str>,
        path: impl AsRef<std::path::Path>,
        filename: Option<impl AsRef<str>>,
        content_type: Option<impl AsRef<str>>,
    ) -> Result<Self> {
        let path = PathBuf::from(path.as_ref());
        self.fields.push(MultipartField::FilePath(
            name.as_ref().to_owned(),
            path,
            filename.map(|f| f.as_ref().to_owned()),
            content_type.map(|c| c.as_ref().to_owned()),
        ));
        Ok(self)
    }

    /// Add an already opened file (auto-detect content-type if filename provided)
    ///
    /// # Arguments
    /// * `name` - Field name
    /// * `file` - Opened file handle
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::body::BodyMultipartForm;
    /// use async_std::fs::File;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let file = File::open("/path/to/avatar.jpg").await?;
    /// let form = BodyMultipartForm::new()
    ///     .add("username", "alice")
    ///     .add_file("avatar", file);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn add_file(mut self, name: impl AsRef<str>, file: File) -> Self {
        self.fields.push(MultipartField::File(
            name.as_ref().to_owned(),
            file,
            None,  // no filename info
            None,  // no content-type info
        ));
        self
    }

    /// Add an already opened file with custom filename and content-type
    ///
    /// # Arguments
    /// * `name` - Field name
    /// * `file` - Opened file handle
    /// * `filename` - Filename (required for content-type detection)
    /// * `content_type` - Content-type (None to auto-detect from filename)
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::body::BodyMultipartForm;
    /// use async_std::fs::File;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let file = File::open("/path/to/image").await?;
    /// let form = BodyMultipartForm::new()
    ///     .add_file_with_options(
    ///         "avatar",
    ///         file,
    ///         Some("profile.jpg"),
    ///         Some("image/jpeg")
    ///     );
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn add_file_with_options(
        mut self,
        name: impl AsRef<str>,
        file: File,
        filename: Option<impl AsRef<str>>,
        content_type: Option<impl AsRef<str>>,
    ) -> Self {
        self.fields.push(MultipartField::File(
            name.as_ref().to_owned(),
            file,
            filename.map(|f| f.as_ref().to_owned()),
            content_type.map(|c| c.as_ref().to_owned()),
        ));
        self
    }

    /// Add a generic stream with filename and content-type
    ///
    /// # Arguments
    /// * `name` - Field name
    /// * `stream` - Stream to read from
    /// * `filename` - Filename (required, used for content-type detection)
    /// * `content_type` - Content-type (None to auto-detect from filename)
    ///
    /// # Examples
    /// ```
    /// use zjhttpc::body::BodyMultipartForm;
    /// use std::io::Cursor;
    ///
    /// let data = b"Hello, world!";
    /// let cursor = Cursor::new(data);
    /// let form = BodyMultipartForm::new()
    ///     .add_stream(
    ///         "data",
    ///         Box::new(cursor),
    ///         Some("data.txt"),
    ///         Some("text/plain")
    ///     );
    /// ```
    #[must_use]
    pub fn add_stream(
        mut self,
        name: impl AsRef<str>,
        stream: Box<dyn async_std::io::Read + Unpin + Send + Sync>,
        filename: Option<impl AsRef<str>>,
        content_type: Option<impl AsRef<str>>,
    ) -> Self {
        self.fields.push(MultipartField::Stream(
            name.as_ref().to_owned(),
            stream,
            filename.map(|f| f.as_ref().to_owned()),
            content_type.map(|c| c.as_ref().to_owned()),
        ));
        self
    }

    /// Get the boundary string for this form
    #[must_use]
    pub fn boundary(&self) -> &str {
        &self.boundary
    }

    /// Get the number of fields in the form
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Check if the form is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

impl Default for BodyMultipartForm {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for BodyMultipartForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BodyMultipartForm")
            .field("boundary", &self.boundary)
            .field("fields_count", &self.fields.len())
            .finish()
    }
}

/// Generate a random boundary string for multipart form data using `rand::rng()`.
fn generate_boundary() -> String {
    use rand::Rng;
    let id: u64 = rand::rng().random();
    format!("----Boundary{id:016x}")
}

/// Detect MIME type based on file extension
pub fn detect_mime_type(filename: &str) -> &'static str {
    if let Some(ext) = filename.rsplit('.').next() {
        match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "pdf" => "application/pdf",
            "txt" => "text/plain",
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "js" => "application/javascript",
            "json" => "application/json",
            "xml" => "application/xml",
            "zip" => "application/zip",
            "rar" => "application/vnd.rar",
            "tar" => "application/x-tar",
            "gz" => "application/gzip",
            "mp3" => "audio/mpeg",
            "mp4" => "video/mp4",
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "webm" => "video/webm",
            "doc" | "docx" => "application/msword",
            "xls" | "xlsx" => "application/vnd.ms-excel",
            "ppt" | "pptx" => "application/vnd.ms-powerpoint",
            _ => "application/octet-stream",
        }
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_body_form_new() {
        let form = BodyForm::new();
        assert!(form.is_empty());
        assert_eq!(form.len(), 0);
    }

    #[test]
    fn test_body_form_add() {
        let form = BodyForm::new()
            .add("username", "alice")
            .add("password", "secret");

        assert_eq!(form.len(), 2);
        assert!(!form.is_empty());
    }

    #[test]
    fn test_body_form_serialize_simple() {
        let form = BodyForm::new()
            .add("username", "alice")
            .add("password", "secret");

        let serialized = form.serialize();
        assert!(serialized.contains("username=alice"));
        assert!(serialized.contains("password=secret"));
    }

    #[test]
    fn test_body_form_serialize_with_spaces() {
        let form = BodyForm::new()
            .add("message", "hello world");

        let serialized = form.serialize();
        assert_eq!(serialized, "message=hello+world");
    }

    #[test]
    fn test_body_form_serialize_with_special_chars() {
        let form = BodyForm::new()
            .add("email", "user@example.com")
            .add("path", "/a/b/c");

        let serialized = form.serialize();
        assert!(serialized.contains("email=user%40example.com"));
        assert!(serialized.contains("path=%2Fa%2Fb%2Fc"));
    }

    #[test]
    fn test_body_form_duplicate_keys() {
        let form = BodyForm::new()
            .add("tags", "rust")
            .add("tags", "http")
            .add("tags", "async");

        let serialized = form.serialize();
        assert_eq!(serialized, "tags=rust&tags=http&tags=async");
    }

    #[test]
    fn test_body_form_chainable() {
        let form = BodyForm::new()
            .add("a", "1")
            .add("b", "2")
            .add("c", "3");

        assert_eq!(form.len(), 3);
    }

    #[test]
    fn test_url_encode_unreserved() {
        assert_eq!(url_encode("abc123-_.~"), "abc123-_.~");
    }

    #[test]
    fn test_url_encode_space() {
        assert_eq!(url_encode("hello world"), "hello+world");
    }

    #[test]
    fn test_url_encode_special() {
        assert_eq!(url_encode("user@example.com"), "user%40example.com");
        assert_eq!(url_encode("/path/to/file"), "%2Fpath%2Fto%2Ffile");
        assert_eq!(url_encode("query=value"), "query%3Dvalue");
    }

    #[test]
    fn test_url_encode_unicode() {
        let encoded = url_encode("你好");
        assert!(encoded.starts_with("%"));
        assert!(encoded.contains("%"));
    }
}
