# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build                    # Build the library
cargo test                     # Run all tests (integration tests hit real network endpoints)
cargo test --test http_client  # Run a specific test file
cargo test test_send_get       # Run a single test by name
cargo run --example body_form  # Run an example
cargo clippy                   # Lint
cargo doc --open               # Generate and view documentation
```

## Architecture

`zjhttpc` is an async HTTP/1.1 client library built on `async-std` + `rustls`. It uses `derive_builder` for the client configuration and `nom` for HTTP response header parsing.

### Request Lifecycle

1. **`ZJHttpClient::send(&self, req: &mut Request)`** (`client.rs`) orchestrates the full flow:
   - Resolve hostname to IP via DNS
   - Acquire a stream from the connection pool or create a new TCP/TLS connection
   - Serialize and write HTTP request headers + body
   - Parse response headers into a `Response` object that wraps the stream for lazy body reading

2. **`Request`** (`requestx.rs`) — Builder for constructing requests. Holds method, URL, headers, query params, cookies, body, and per-request timeout/proxy overrides.

3. **`Response`** (`response.rs`) — Wraps the response stream. Body is read on demand via `body_string()`, `body_bytes()`, or `body_json()`. Tracks completion via an `AtomicBool` to determine when the underlying stream can be returned to the connection pool.

### Connection Pooling

A per-client `ConnectionPoolInner` (in `client.rs`) pools connections keyed by `(SocketAddr, ConnectionType)` in a `DashMap`. Each entry tracks `PooledConnection { stream, returned_at: Instant }` for idle eviction. The pool enforces:
- **Per-key limit**: max connections per `(addr, connection_type)` (default 30)
- **Global limit**: max total connections across all keys (default 1000)
- **Idle timeout**: connections older than the timeout are discarded on pick/return (default 90s)
- **Empty entry cleanup**: DashMap entries are removed when their Vec is drained

Pool config is set via `ZJHttpClient::set_pool_config(max_per_key, max_total, idle_timeout)`. The pool is self-contained (config travels with the Arc), so Response and stream wrappers only need the pool reference.

### Stream Abstraction

`stream.rs` defines `RWStream` trait and `BoxedStream` (type-erased box) that unifies TCP streams (`async_std::net::TcpStream`) and TLS streams (`async_tls::client::TlsStream`) behind a single interface.

### Body Handling

`body.rs` supports:
- URL-encoded forms (`BodyForm`) — uses `indexmap::IndexMap` to preserve insertion order and allow duplicate keys
- Multipart forms (`BodyMultipartForm`) with file uploads and auto MIME detection
- Raw bytes, strings, and streaming bodies

### Proxy Support

`proxy.rs` implements HTTP CONNECT proxy tunneling. `HttpsProxyOption` holds proxy URL, auth, and TLS config. Proxy connections are pooled separately (keyed by proxy address).

### Error Handling

Mixed approach: `ZjhttpcError` (`error.rs`) uses `thiserror` for typed HTTP parsing errors, while the client code uses `anyhow_ext` for contextual error chains with `.dot()` extension.

### Re-exports

`lib.rs` re-exports `url` crate so consumers don't need to add it as a separate dependency. Public modules: `body`, `client`, `content_type`, `cookie`, `error`, `header`, `methods`, `misc`, `proxy`, `requestx`, `response`, `stream`.

## Key Dependencies

- `async-std` — async runtime (not tokio)
- `async-tls` + `rustls` — TLS (no OpenSSL dependency)
- `dashmap` — concurrent connection pool
- `nom` — HTTP response header parsing
- `derive_builder` — client struct builder
- `encoding_rs` — charset support including GBK
- `snafu` — used alongside anyhow/thiserror in some modules

## Notes

- Rust edition 2024
- Tests in `tests/` are integration tests that make real HTTP requests to external servers
- `examples/` directory contains runnable usage demos
