use std::any::Any;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_std::{io, net::TcpStream};
use async_tls::client::TlsStream;

pub trait AsAny {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
pub trait RWStream: io::Read + io::Write + Unpin + Sync + Send + 'static {}
impl<T: Any + RWStream> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        return self;
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        return self;
    }
}

impl RWStream for TcpStream {}
impl RWStream for TlsStream<TcpStream> {}
impl RWStream for TlsStream<BoxedStream> {}
pub trait AnyStream: RWStream + AsAny {}
impl<T: RWStream + AsAny> AnyStream for T {}
pub type BoxedStream = Box<dyn AnyStream>;

/// Read-only boxed stream. Used for response body streams that don't need Write.
pub type ReadStream = Box<dyn async_std::io::Read + Unpin + Send + Sync>;

/// Chains two async `Read` streams: reads `first` to EOF, then reads `second`.
pub struct ChainRead<A, B> {
    first: Option<A>,
    second: B,
}

impl<A, B> ChainRead<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self {
            first: Some(first),
            second,
        }
    }

    pub fn into_second(self) -> B {
        self.second
    }
}

impl<A: io::Read + Unpin, B: io::Read + Unpin> io::Read for ChainRead<A, B> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if let Some(first) = &mut self.first {
            match Pin::new(first).poll_read(cx, buf) {
                Poll::Ready(Ok(0)) => {
                    self.first = None;
                }
                other => return other,
            }
        }
        Pin::new(&mut self.second).poll_read(cx, buf)
    }
}

/// A trivial async `Read` over a byte slice (no heap allocation).
pub struct SliceRead {
    data: [u8; 4096],
    len: usize,
    pos: usize,
}

impl SliceRead {
    pub fn new(data: &[u8]) -> Self {
        let mut buf = [0u8; 4096];
        let len = data.len().min(4096);
        buf[..len].copy_from_slice(&data[..len]);
        Self {
            data: buf,
            len,
            pos: 0,
        }
    }
}

impl io::Read for SliceRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.pos >= self.len {
            return Poll::Ready(Ok(0));
        }
        let n = std::cmp::min(self.len - self.pos, buf.len());
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Poll::Ready(Ok(n))
    }
}
