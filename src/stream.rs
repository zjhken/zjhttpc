use std::any::Any;

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

impl RWStream for TcpStream{}
impl RWStream for TlsStream<TcpStream> {}
pub trait AnyStream: RWStream + AsAny {}
impl<T: RWStream + AsAny> AnyStream for T {}
pub type BoxedStream = Box<dyn AnyStream>;

