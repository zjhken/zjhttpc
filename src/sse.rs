//! Server-Sent Events (SSE) parser.
//!
//! [`SseStream`] is a consumer layered on top of [`crate::stream::ReadStream`]
//! (the output of [`crate::response::Response::body_managed_stream`]). The
//! underlying managed-stream wrapper still owns chunked decoding, framing, EOF
//! detection, and connection-pool return; this module only adds line buffering
//! and SSE field parsing.
//!
//! See <https://html.spec.whatwg.org/multipage/server-sent-events.html> for the
//! wire format.

use async_std::io::ReadExt;

use crate::{
    error::Result,
    stream::ReadStream,
};

/// One dispatched SSE event.
///
/// Built up from one or more field lines and dispatched on the blank line that
/// terminates the event.
#[derive(Debug)]
pub struct SseEvent {
    /// Value of the last `event:` field in this event, if any. `None` means the
    /// consumer should treat the event as the default type (`"message"`).
    pub event: Option<String>,
    /// All `data:` lines joined with `\n`, with a trailing `\n` appended on
    /// dispatch (per spec).
    pub data: String,
    /// Value of the last `id:` field in this event, if any.
    pub id: Option<String>,
    /// Value of the last `retry:` field in this event, parsed as milliseconds.
    /// `None` if absent or not a valid integer.
    pub retry: Option<u64>,
}

/// Streaming SSE parser wrapping a [`ReadStream`].
///
/// Call [`SseStream::next_event`] repeatedly to receive events as the server
/// sends them. When the server closes the stream, `next_event` returns
/// `Ok(None)`.
pub struct SseStream {
    inner: ReadStream,
    byte_buf: Vec<u8>,
    event_type: Option<String>,
    data_lines: Vec<String>,
    last_event_id: Option<String>,
    retry: Option<u64>,
}

impl SseStream {
    pub fn new(inner: ReadStream) -> Self {
        Self {
            inner,
            byte_buf: Vec::new(),
            event_type: None,
            data_lines: Vec::new(),
            last_event_id: None,
            retry: None,
        }
    }

    /// Returns the next dispatched event, or `Ok(None)` when the underlying
    /// stream reaches EOF.
    ///
    /// A partial event still buffered when EOF arrives is discarded (the spec
    /// requires a blank line to dispatch).
    pub async fn next_event(&mut self) -> Result<Option<SseEvent>> {
        let mut chunk = [0u8; 1024];
        loop {
            if let Some(event) = self.try_drain_one_event(false) {
                return Ok(Some(event));
            }
            let n = self.inner.read(&mut chunk).await?;
            if n == 0 {
                // EOF: a trailing lone `\r` can now be treated as a complete
                // terminator, since no `\n` can follow.
                return Ok(self.try_drain_one_event(true));
            }
            self.byte_buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Pulls complete lines out of `byte_buf` and feeds them to `process_line`
    /// until either an event is dispatched or the buffer has no more complete
    /// lines.
    ///
    /// `at_eof` controls how a trailing lone `\r` is interpreted: when false,
    /// it's treated as incomplete (more bytes needed to know if `\n` follows);
    /// when true, it's treated as a lone-CR terminator.
    fn try_drain_one_event(&mut self, at_eof: bool) -> Option<SseEvent> {
        loop {
            let (line_end, term_len) = find_line_terminator(&self.byte_buf, at_eof)?;
            let line: Vec<u8> = self.byte_buf.drain(..line_end).collect();
            self.byte_buf.drain(..term_len);
            if let Some(event) = process_line(
                line,
                &mut self.event_type,
                &mut self.data_lines,
                &mut self.last_event_id,
                &mut self.retry,
            ) {
                return Some(event);
            }
        }
    }
}

/// Locate the next line terminator in `buf`.
///
/// Returns `(line_end, term_len)` where `line_end` is the index of the first
/// byte of the terminator and `term_len` is the number of bytes that make up
/// the terminator (1 for `\n` or lone `\r`, 2 for `\r\n`).
///
/// Returns `None` if no complete terminator is present. A trailing lone `\r`
/// at the last byte is treated as incomplete unless `at_eof` is true — we need
/// one more byte to know whether `\n` follows.
fn find_line_terminator(buf: &[u8], at_eof: bool) -> Option<(usize, usize)> {
    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            return Some((i, 1));
        }
        if b == b'\r' {
            if i + 1 < buf.len() {
                if buf[i + 1] == b'\n' {
                    return Some((i, 2));
                }
                return Some((i, 1));
            }
            return if at_eof { Some((i, 1)) } else { None };
        }
    }
    None
}

/// Apply one SSE line to the accumulators. Returns `Some(event)` if this line
/// was the blank-line dispatch trigger and an event should be emitted.
#[allow(clippy::too_many_arguments)]
fn process_line(
    line: Vec<u8>,
    event_type: &mut Option<String>,
    data_lines: &mut Vec<String>,
    last_event_id: &mut Option<String>,
    retry: &mut Option<u64>,
) -> Option<SseEvent> {
    if line.is_empty() {
        if data_lines.is_empty() {
            *event_type = None;
            *last_event_id = None;
            *retry = None;
            return None;
        }
        let mut data = data_lines.join("\n");
        data.push('\n');
        let event = SseEvent {
            event: event_type.take(),
            data,
            id: last_event_id.take(),
            retry: retry.take(),
        };
        data_lines.clear();
        return Some(event);
    }

    if line[0] == b':' {
        return None;
    }

    let (field, value) = split_field_value(&line);
    let field = String::from_utf8_lossy(field).into_owned();
    let value = String::from_utf8_lossy(value).into_owned();

    match field.as_str() {
        "data" => data_lines.push(value),
        "event" => *event_type = if value.is_empty() { None } else { Some(value) },
        "id" => *last_event_id = Some(value),
        "retry" => {
            if let Ok(ms) = value.parse::<u64>() {
                *retry = Some(ms);
            }
        }
        _ => {}
    }
    None
}

/// Split a non-empty line at the first colon. Strips exactly one leading
/// U+0020 SPACE from the value if present. If there is no colon, the whole
/// line is the field and the value is empty.
fn split_field_value(line: &[u8]) -> (&[u8], &[u8]) {
    match line.iter().position(|&b| b == b':') {
        Some(idx) => {
            let field = &line[..idx];
            let mut value_start = idx + 1;
            if value_start < line.len() && line[value_start] == b' ' {
                value_start += 1;
            }
            (field, &line[value_start..])
        }
        None => (line, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ZjhttpcError;

    fn mock_stream(data: &[u8]) -> ReadStream {
        struct MockStream {
            data: Vec<u8>,
            pos: usize,
            chunk_size: usize,
        }
        impl async_std::io::Read for MockStream {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                let remaining = self.data.len() - self.pos;
                if remaining == 0 {
                    return std::task::Poll::Ready(Ok(0));
                }
                let n = std::cmp::min(buf.len(), std::cmp::min(remaining, self.chunk_size));
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                std::task::Poll::Ready(Ok(n))
            }
        }
        // Default: feed all bytes at once.
        let s = MockStream {
            data: data.to_vec(),
            pos: 0,
            chunk_size: data.len().max(1),
        };
        Box::new(s)
    }

    fn mock_stream_chunked(data: &[u8], chunk_size: usize) -> ReadStream {
        struct MockChunked {
            data: Vec<u8>,
            pos: usize,
            chunk_size: usize,
        }
        impl async_std::io::Read for MockChunked {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                if self.pos >= self.data.len() {
                    return std::task::Poll::Ready(Ok(0));
                }
                let n = std::cmp::min(buf.len(), std::cmp::min(self.chunk_size, self.data.len() - self.pos));
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                std::task::Poll::Ready(Ok(n))
            }
        }
        Box::new(MockChunked {
            data: data.to_vec(),
            pos: 0,
            chunk_size,
        })
    }

    async fn one_event(stream: &mut SseStream) -> Option<SseEvent> {
        stream.next_event().await.unwrap()
    }

    #[async_std::test]
    async fn single_event_basic() {
        let mut s = SseStream::new(mock_stream(b"data: hello\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "hello\n");
        assert_eq!(ev.event, None);
        assert_eq!(ev.id, None);
        assert_eq!(ev.retry, None);
    }

    #[async_std::test]
    async fn multi_data_lines() {
        let mut s = SseStream::new(mock_stream(b"data: a\ndata: b\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "a\nb\n");
    }

    #[async_std::test]
    async fn custom_event_type() {
        let mut s = SseStream::new(mock_stream(b"event: update\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.event.as_deref(), Some("update"));
    }

    #[async_std::test]
    async fn id_field() {
        let mut s = SseStream::new(mock_stream(b"id: 42\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.id.as_deref(), Some("42"));
    }

    #[async_std::test]
    async fn retry_field() {
        let mut s = SseStream::new(mock_stream(b"retry: 5000\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.retry, Some(5000));
    }

    #[async_std::test]
    async fn comment_ignored() {
        let mut s = SseStream::new(mock_stream(b": keepalive\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "x\n");
    }

    #[async_std::test]
    async fn crlf_line_endings() {
        let mut s = SseStream::new(mock_stream(b"data: hi\r\n\r\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "hi\n");
    }

    #[async_std::test]
    async fn cr_only_line_endings() {
        let mut s = SseStream::new(mock_stream(b"data: hi\r\r"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "hi\n");
    }

    #[async_std::test]
    async fn no_colon_line() {
        let mut s = SseStream::new(mock_stream(b"data\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "\n");
    }

    #[async_std::test]
    async fn leading_space_stripped_once() {
        // "data:   hi" — value after colon is "  hi" (3 spaces + hi). Spec strips one space.
        let mut s = SseStream::new(mock_stream(b"data:   hi\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "  hi\n");
    }

    #[async_std::test]
    async fn empty_data_value() {
        let mut s = SseStream::new(mock_stream(b"data:\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "\n");
    }

    #[async_std::test]
    async fn retry_non_numeric_ignored() {
        let mut s = SseStream::new(mock_stream(b"retry: abc\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.retry, None);
        assert_eq!(ev.data, "x\n");
    }

    #[async_std::test]
    async fn unknown_field_ignored() {
        let mut s = SseStream::new(mock_stream(b"foo: bar\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "x\n");
    }

    #[async_std::test]
    async fn dispatch_without_data_no_event() {
        // No data line — blank line resets silently. Then EOF.
        let mut s = SseStream::new(mock_stream(b"event: foo\n\n"));
        let ev = s.next_event().await.unwrap();
        assert!(ev.is_none());
    }

    #[async_std::test]
    async fn eof_mid_event_no_dispatch() {
        let mut s = SseStream::new(mock_stream(b"data: hi"));
        let ev = s.next_event().await.unwrap();
        assert!(ev.is_none());
    }

    #[async_std::test]
    async fn eof_after_cr_no_dispatch() {
        // Trailing lone \r at EOF with no complete terminator should not dispatch.
        let mut s = SseStream::new(mock_stream(b"data: hi\r"));
        let ev = s.next_event().await.unwrap();
        assert!(ev.is_none());
    }

    #[async_std::test]
    async fn multiple_events_in_one_buffer() {
        let mut s = SseStream::new(mock_stream(b"data: a\n\ndata: b\n\n"));
        let ev1 = one_event(&mut s).await.unwrap();
        let ev2 = one_event(&mut s).await.unwrap();
        assert_eq!(ev1.data, "a\n");
        assert_eq!(ev2.data, "b\n");
        assert!(s.next_event().await.unwrap().is_none());
    }

    #[async_std::test]
    async fn incremental_reads_one_byte_at_a_time() {
        let mut s = SseStream::new(mock_stream_chunked(b"data: hello\n\n", 1));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "hello\n");
    }

    #[async_std::test]
    async fn utf8_across_chunk_boundary() {
        // "data: héllo\n\n" — é is 0xC3 0xA9 (two bytes). Split mid-char must not corrupt.
        let bytes = "data: héllo\n\n".as_bytes();
        let mut s = SseStream::new(mock_stream_chunked(bytes, 1));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.data, "héllo\n");
    }

    #[async_std::test]
    async fn event_field_overwritten_within_event() {
        let mut s = SseStream::new(mock_stream(b"event: a\nevent: b\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.event.as_deref(), Some("b"));
    }

    #[async_std::test]
    async fn empty_event_field_resets_to_default() {
        let mut s = SseStream::new(mock_stream(b"event: a\nevent:\ndata: x\n\n"));
        let ev = one_event(&mut s).await.unwrap();
        assert_eq!(ev.event, None);
    }

    #[test]
    fn find_line_terminator_lf() {
        assert_eq!(find_line_terminator(b"abc\n", false), Some((3, 1)));
    }

    #[test]
    fn find_line_terminator_crlf() {
        assert_eq!(find_line_terminator(b"abc\r\n", false), Some((3, 2)));
    }

    #[test]
    fn find_line_terminator_cr_only() {
        assert_eq!(find_line_terminator(b"abc\rdef", false), Some((3, 1)));
    }

    #[test]
    fn find_line_terminator_trailing_cr_waits() {
        // Trailing lone \r is incomplete — we need one more byte.
        assert_eq!(find_line_terminator(b"abc\r", false), None);
    }

    #[test]
    fn find_line_terminator_trailing_cr_at_eof() {
        // At EOF, a trailing lone \r is recognized as a terminator.
        assert_eq!(find_line_terminator(b"abc\r", true), Some((3, 1)));
    }

    #[test]
    fn find_line_terminator_none() {
        assert_eq!(find_line_terminator(b"abc", false), None);
    }

    #[test]
    fn split_field_value_basic() {
        assert_eq!(split_field_value(b"data: hello"), (b"data".as_slice(), b"hello".as_slice()));
    }

    #[test]
    fn split_field_value_strips_one_space() {
        assert_eq!(split_field_value(b"data:  two"), (b"data".as_slice(), b" two".as_slice()));
    }

    #[test]
    fn split_field_value_no_space() {
        assert_eq!(split_field_value(b"data:hello"), (b"data".as_slice(), b"hello".as_slice()));
    }

    #[test]
    fn split_field_value_no_colon() {
        assert_eq!(split_field_value(b"data"), (b"data".as_slice(), b"".as_slice()));
    }

    #[test]
    fn split_field_value_empty_value() {
        assert_eq!(split_field_value(b"data:"), (b"data".as_slice(), b"".as_slice()));
    }

    // Touch the unused-error-path assertion: read errors propagate.
    struct ErroringStream;
    impl async_std::io::Read for ErroringStream {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "boom",
            )))
        }
    }

    #[async_std::test]
    async fn read_error_propagates() {
        let mut s = SseStream::new(Box::new(ErroringStream));
        let err = s.next_event().await.unwrap_err();
        match err {
            ZjhttpcError::Io { source: arc, .. } => assert!(arc.to_string().contains("boom")),
            other => panic!("expected Io error, got {other:?}"),
        }
    }
}
