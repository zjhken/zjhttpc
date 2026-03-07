/// HTTP Cookie representation with attributes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
}

impl Cookie {
    /// Create a new cookie with name and value
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Cookie {
            name: name.into(),
            value: value.into(),
        }
    }

    /// Parse cookies from Set-Cookie header values
    ///
    /// # Arguments
    /// * `set_cookie_values` - Iterator of Set-Cookie header values
    ///
    /// # Returns
    /// Vec of parsed cookies
    pub fn parse_from_set_cookie<'a, I>(set_cookie_values: I) -> Vec<Self>
    where
        I: IntoIterator<Item = &'a str>,
    {
        set_cookie_values
            .into_iter()
            .filter_map(|value| Self::parse_one(value))
            .collect()
    }

    /// Parse a single Set-Cookie header value
    fn parse_one(set_cookie_value: &str) -> Option<Self> {
        // Set-Cookie format: "name=value; Attribute1; Attribute2"
        // We only care about the name=value part
        let trimmed = set_cookie_value.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Split by ';' to separate name=value from attributes
        let first_part = trimmed.split(';').next()?;

        // Split by '=' to get name and value
        let mut parts = first_part.splitn(2, '=');
        let name = parts.next()?.trim();
        let value = parts.next().unwrap_or("").trim();

        if name.is_empty() {
            return None;
        }

        Some(Cookie {
            name: name.to_string(),
            value: value.to_string(),
        })
    }

    /// Format cookies for Cookie header
    /// Converts Vec<Cookie> to "name=value; name2=value2" format
    pub fn format_for_request_cookie_header(cookies: &[Self]) -> String {
        cookies
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_cookie() {
        let cookies = Cookie::parse_from_set_cookie(vec!["sessionid=abc123"]);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "sessionid");
        assert_eq!(cookies[0].value, "abc123");
    }

    #[test]
    fn test_parse_cookie_with_attributes() {
        let cookies = Cookie::parse_from_set_cookie(vec![
            "sessionid=abc123; Path=/; HttpOnly; Secure",
        ]);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "sessionid");
        assert_eq!(cookies[0].value, "abc123");
    }

    #[test]
    fn test_parse_multiple_cookies() {
        let cookies = Cookie::parse_from_set_cookie(vec![
            "sessionid=abc123; HttpOnly",
            "userdata=eyJ1c2VyIjoiYWxpY2UifQ==",
        ]);
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "sessionid");
        assert_eq!(cookies[1].name, "userdata");
    }

    #[test]
    fn test_parse_cookie_with_empty_value() {
        let cookies = Cookie::parse_from_set_cookie(vec!["empty=; Path=/"]);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "empty");
        assert_eq!(cookies[0].value, "");
    }

    #[test]
    fn test_parse_cookie_with_equals_in_value() {
        let cookies = Cookie::parse_from_set_cookie(vec![
            "token=abc=123=def; Secure",
        ]);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "token");
        assert_eq!(cookies[0].value, "abc=123=def");
    }

    #[test]
    fn test_format_for_cookie_header() {
        let cookies = vec![
            Cookie::new("sessionid", "abc123"),
            Cookie::new("userdata", "eyJ1c2VyIjoiYWxpY2UifQ=="),
        ];
        let formatted = Cookie::format_for_request_cookie_header(&cookies);
        assert_eq!(formatted, "sessionid=abc123; userdata=eyJ1c2VyIjoiYWxpY2UifQ==");
    }

    #[test]
    fn test_parse_empty_input() {
        let cookies = Cookie::parse_from_set_cookie(vec![""]);
        assert_eq!(cookies.len(), 0);
    }

    #[test]
    fn test_cookie_new() {
        let cookie = Cookie::new("test", "value");
        assert_eq!(cookie.name, "test");
        assert_eq!(cookie.value, "value");
    }
}
