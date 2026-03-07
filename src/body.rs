use std::fmt;

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
