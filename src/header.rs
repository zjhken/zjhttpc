// Common HTTP header name constants
// These provide type-safe, documented header names for common HTTP headers
// Reference: https://www.iana.org/assignments/message-headers/message-headers.xhtml

// ========== Common Request Headers ==========

/// Accept header - specifies media types the client can understand
/// Example: `Accept: text/html, application/json`
pub const ACCEPT: &str = "Accept";

/// Accept-Charset header - specifies character sets the client can understand
/// Example: `Accept-Charset: utf-8, iso-8859-1`
pub const ACCEPT_CHARSET: &str = "Accept-Charset";

/// Accept-Encoding header - specifies content encodings the client can understand
/// Example: `Accept-Encoding: gzip, deflate, br`
pub const ACCEPT_ENCODING: &str = "Accept-Encoding";

/// Accept-Language header - specifies natural languages the client prefers
/// Example: `Accept-Language: en-US, en;q=0.9`
pub const ACCEPT_LANGUAGE: &str = "Accept-Language";

/// Authorization header - contains authentication credentials
/// Example: `Authorization: Bearer <token>`
pub const AUTHORIZATION: &str = "Authorization";

/// Cache-Control header - specifies directives for caching mechanisms
/// Example: `Cache-Control: no-cache`
pub const CACHE_CONTROL: &str = "Cache-Control";

/// Connection header - controls whether the network connection stays open after the current transaction
/// Example: `Connection: keep-alive`
pub const CONNECTION: &str = "Connection";

/// Content-Length header - indicates the size of the entity-body in bytes
/// Example: `Content-Length: 1234`
pub const CONTENT_LENGTH: &str = "Content-Length";

/// Content-Type header - indicates the media type of the resource
/// Example: `Content-Type: application/json`
pub const CONTENT_TYPE: &str = "Content-Type";

/// Cookie header - contains stored HTTP cookies previously sent by the server
/// Example: `Cookie: name=value; name2=value2`
pub const COOKIE: &str = "Cookie";

/// Date header - the date and time at which the message was originated
/// Example: `Date: Wed, 07 Mar 2026 12:00:00 GMT`
pub const DATE: &str = "Date";

/// Expect header - indicates expectations that need to be fulfilled by the server
/// Example: `Expect: 100-continue`
pub const EXPECT: &str = "Expect";

/// From header - contains an email address for the user making the request
/// Example: `From: user@example.com`
pub const FROM: &str = "From";

/// Host header - specifies the domain name and port number of the server
/// Example: `Host: www.example.com:8080`
pub const HOST: &str = "Host";

/// If-Match header - makes the request conditional based on ETag
/// Example: `If-Match: "737060cd8c284d8af7ad3082f209582d"`
pub const IF_MATCH: &str = "If-Match";

/// If-Modified-Since header - makes the request conditional based on modification date
/// Example: `If-Modified-Since: Wed, 07 Mar 2026 12:00:00 GMT`
pub const IF_MODIFIED_SINCE: &str = "If-Modified-Since";

/// If-None-Match header - makes the request conditional based on ETag absence
/// Example: `If-None-Match: "737060cd8c284d8af7ad3082f209582d"`
pub const IF_NONE_MATCH: &str = "If-None-Match";

/// If-Range header - makes the request conditional based on range existence
/// Example: `If-Range: "737060cd8c284d8af7ad3082f209582d"`
pub const IF_RANGE: &str = "If-Range";

/// If-Unmodified-Since header - makes the request conditional based on no modifications
/// Example: `If-Unmodified-Since: Wed, 07 Mar 2026 12:00:00 GMT`
pub const IF_UNMODIFIED_SINCE: &str = "If-Unmodified-Since";

/// Max-Forwards header - indicates the maximum number of hops the request can make
/// Example: `Max-Forwards: 10`
pub const MAX_FORWARDS: &str = "Max-Forwards";

/// Origin header - indicates the origin of the CORS request
/// Example: `Origin: https://example.com`
pub const ORIGIN: &str = "Origin";

/// Pragma header - implementation-specific header that may have various effects
/// Example: `Pragma: no-cache`
pub const PRAGMA: &str = "Pragma";

/// Proxy-Authorization header - contains authorization credentials for the proxy
/// Example: `Proxy-Authorization: Basic <credentials>`
pub const PROXY_AUTHORIZATION: &str = "Proxy-Authorization";

/// Range header - requests only part of an entity
/// Example: `Range: bytes=0-1023`
pub const RANGE: &str = "Range";

/// Referer header - contains the address of the previous web page
/// Example: `Referer: https://example.com/page`
pub const REFERER: &str = "Referer";

/// TE header - specifies transfer encodings the user agent is willing to accept
/// Example: `TE: trailers, deflate`
pub const TE: &str = "TE";

/// Trailer header - indicates that the given set of header fields is present in the trailer
/// Example: `Trailer: Expires`
pub const TRAILER: &str = "Trailer";

/// Transfer-Encoding header - specifies the form of encoding used to transfer the message
/// Example: `Transfer-Encoding: chunked`
pub const TRANSFER_ENCODING: &str = "Transfer-Encoding";

/// Upgrade header - asks the server to upgrade to another protocol
/// Example: `Upgrade: h2c, HTTP/2.0`
pub const UPGRADE: &str = "Upgrade";

/// User-Agent header - contains a characteristic string for application identification
/// Example: `User-Agent: Mozilla/5.0`
pub const USER_AGENT: &str = "User-Agent";

/// Via header - added by proxies to track intermediate hops
/// Example: `Via: 1.1 proxy1, 1.1 proxy2`
pub const VIA: &str = "Via";

/// Warning header - general warning information about possible problems
/// Example: `Warning: 199 Miscellaneous warning`
pub const WARNING: &str = "Warning";

// ========== Common Response Headers ==========

/// Accept-Ranges header - indicates what partial content range types the server supports
/// Example: `Accept-Ranges: bytes`
pub const ACCEPT_RANGES: &str = "Accept-Ranges";

/// Age header - indicates the time in seconds the object has been in a proxy cache
/// Example: `Age: 3600`
pub const AGE: &str = "Age";

/// Allow header - lists the set of methods supported by the resource
/// Example: `Allow: GET, HEAD, POST`
pub const ALLOW: &str = "Allow";

/// Content-Disposition header - indicates if the content is expected to be displayed inline or as an attachment
/// Example: `Content-Disposition: attachment; filename="file.txt"`
pub const CONTENT_DISPOSITION: &str = "Content-Disposition";

/// Content-Encoding header - used to specify what additional content encodings have been applied
/// Example: `Content-Encoding: gzip`
pub const CONTENT_ENCODING: &str = "Content-Encoding";

/// Content-Language header - describes the natural language(s) of the intended audience
/// Example: `Content-Language: en-US`
pub const CONTENT_LANGUAGE: &str = "Content-Language";

/// Content-Location header - indicates an alternate location for the returned data
/// Example: `Content-Location: /index.html`
pub const CONTENT_LOCATION: &str = "Content-Location";

/// Content-Range header - indicates where in a full body message a partial message belongs
/// Example: `Content-Range: bytes 0-1023/2048`
pub const CONTENT_RANGE: &str = "Content-Range";

/// ETag header - an identifier for a specific version of a resource
/// Example: `ETag: "737060cd8c284d8af7ad3082f209582d"`
pub const ETAG: &str = "ETag";

/// Expires header - contains the date/time after which the response is considered stale
/// Example: `Expires: Wed, 07 Mar 2026 12:00:00 GMT`
pub const EXPIRES: &str = "Expires";

/// Last-Modified header - indicates the last modification date of the resource
/// Example: `Last-Modified: Wed, 07 Mar 2026 12:00:00 GMT`
pub const LAST_MODIFIED: &str = "Last-Modified";

/// Location header - used in redirection or when a new resource has been created
/// Example: `Location: https://example.com/new-page`
pub const LOCATION: &str = "Location";

/// Proxy-Authenticate header - defines the authentication method for the proxy
/// Example: `Proxy-Authenticate: Basic`
pub const PROXY_AUTHENTICATE: &str = "Proxy-Authenticate";

/// Refresh header - specifies the time delay before the web browser should refresh the page
/// Example: `Refresh: 5; url=https://example.com`
pub const REFRESH: &str = "Refresh";

/// Retry-After header - indicates how long the user agent should wait before making a follow-up request
/// Example: `Retry-After: 120`
pub const RETRY_AFTER: &str = "Retry-After";

/// Server header - contains information about the software used by the origin server
/// Example: `Server: Apache/2.4.41`
pub const SERVER: &str = "Server";

/// Set-Cookie header - used to send cookies from the server to the user agent
/// Example: `Set-Cookie: name=value; HttpOnly`
pub const SET_COOKIE: &str = "Set-Cookie";

/// Vary header - determines how to match future request headers to decide whether a cached response can be used
/// Example: `Vary: Accept-Encoding`
pub const VARY: &str = "Vary";

/// WWW-Authenticate header - indicates the authentication scheme for the resource
/// Example: `WWW-Authenticate: Bearer realm="example"`
pub const WWW_AUTHENTICATE: &str = "WWW-Authenticate";

// TODO: implement general Headers struct