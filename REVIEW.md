# zjhttpc Code Review

## Security Issues

### 1. HTTP Header Injection (CRLF injection)

`client.rs:579-593` — Request path, header key/value are written directly to stream without validating `\r\n`. A malicious URL or header value can inject extra HTTP headers.

```rust
stream.write_all(req.url.path().as_bytes()).await.dot()?;  // path contains \r\n?
stream.write_all(key.as_bytes()).await.dot()?;             // key contains \r\n?
stream.write_all(value.as_bytes()).await.dot()?;           // value contains \r\n?
```

Multipart `name` and `filename` have the same issue (`client.rs:702-739`).

### 2. Multipart boundary generation is insecure

`body.rs:427-432` — `rand_random_string` uses `SystemTime::now()` for randomness. Within the loop there is almost no delay, so 8 consecutive calls will likely return the same nanosecond value, producing repeated characters. The boundary is predictable and collision-prone.

```rust
for _ in 0..len {
    let idx = (SystemTime::now().duration_since(UNIX_EPOCH)... % CHARS.len()) as usize;
    // consecutive SystemTime::now() calls return nearly the same value
}
```

Should use the already-imported `rand::rng()` instead.

### 3. Library should not panic — `load_native_certs().expect()`

`client.rs:529`, `proxy.rs:304` — `load_native_certs().expect()` will panic if system certs cannot be loaded. As a library, this decision should be left to the caller. Should return `Result` instead.

---

## Functional Bugs

### 4. Always sends `Content-Length: 0`

`client.rs:608-613` — `Content-Length` is written regardless of body type. For multipart forms (`requestx.rs:280`), `content_length` is set to 0. The code comment says "will use chunked encoding" but `Transfer-Encoding: chunked` is never actually sent, causing the server to receive `Content-Length: 0` and ignore the body.

### 5. `body_prefix` 4096-byte truncation loses data

`response.rs:572-574` + `stream.rs:77-80` — Both locations have a hardcoded 4096-byte limit:
- `SliceRead::new()` silently truncates prefix data exceeding 4096 bytes
- `new_from_parse_result()` does the same

If `read_until` reads more than 4096 bytes of overflow during header parsing, the beginning of the body data will be silently lost.

### 6. Does not handle `Connection: close`

The client always sends `Connection: keep-alive` but never checks the server's `Connection: close` response header. If the server responds with close, the connection is still returned to the pool, causing subsequent requests to use a closed connection.

### 7. Header parsing does not support `Header:value` (no space after colon)

`client.rs:921-928` — `parse_one_line_header` uses `tag(": ")` requiring a space after the colon. The HTTP spec allows `Header:value`, so this will cause parsing failure. Test `test_parse_one_line_header_empty_value_no_space` already documents this issue.

### 8. `expect_continue` implementation is fragile

`client.rs:631-646` — Uses exact string match `"HTTP/1.1 100 Continue\r\n\r\n"`. Servers may use `HTTP/1.0`, extra whitespace, or different reason phrases.

### 9. `> 512` check in `read_connect_response` is dead code

`proxy.rs:288` — When `filled == 512`, `buf[filled..]` is an empty slice, `read` returns 0, triggering the EOF error immediately. `filled > 512` can never be true.

---

## Design Issues

### 10. `Ordering::Relaxed` for completion flag

Throughout `response.rs` — All `AtomicBool` load/store use `Relaxed`. If the stream completes in one task and Response is dropped in another, `Relaxed` does not guarantee visibility. Should use at least `Acquire`/`Release`.

### 11. Three body stream types with highly duplicated code

`ChunkedDecoderStream`, `BodyFixedLengthStream`, `BodyUnknownLengthStream` share identical fields (`addr`, `is_tls`, `proxy_used`, `pool`, `completion_flag`) and `return_stream_to_pool` logic. Should be abstracted via generics or composition.

### 12. `Request` method is `&'static str`

`requestx.rs:21` — Cannot be constructed from dynamic strings, which is inflexible. Should be `String` or `Cow<'static, str>`.

### 13. No response compression support

Does not send `Accept-Encoding` header, nor handle `Content-Encoding: gzip/deflate/br`. Wastes bandwidth for large responses.

### 14. No 3xx redirect following

301/302/307/308 responses are returned directly without automatic redirect following. Should at least provide optional support.

### 15. `HttpsProxyOption` naming is misleading

The struct handles both HTTP and HTTPS proxies, but the name suggests HTTPS-only.

---

## Minor Issues

### 16. Duplicate `HttpVersion` definition

`misc.rs:4-7` and `client.rs:997-1000` each define a copy. The one in `misc.rs` is unused.

### 17. Unused error variant

`error.rs:17` — `BodyHasBeenRead` is defined but the code uses `anyhow!("response body has been read")` instead.

### 18. Unused dependencies

`Cargo.toml` — `snafu` and `thiserror-context` are listed as dependencies but not used in the code. `hashbrown` could be replaced with `std::collections::HashMap` (performance difference is negligible in this scenario).

### 19. `TestStream`/`MockStream` defined 6+ times in tests

Test modules in `response.rs` and `client.rs` each independently define nearly identical mock stream structs.

### 20. `rustls 0.21` is outdated

`Cargo.toml:23` — 0.21 is from 2023. Current rustls is 0.23+ with significant API changes.

### 21. Missing `#[non_exhaustive]`

Public enums `Body`, `MultipartField`, `ZjhttpcError`, `TrustStorePem` lack `#[non_exhaustive]`, so adding variants in the future will be a breaking change.

---

## Priority Recommendations

**Fix first**: #1 (CRLF injection), #4 (multipart body always empty), #6 (Connection: close not handled). These directly affect correctness and security in production.
