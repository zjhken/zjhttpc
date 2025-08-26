

pub enum HttpVersion {
    V1_0,
    V1_1,
}

pub enum Body {
    Str(String),
    Stream(Box<dyn async_std::io::Read + Unpin + Send + Sync>),
    ByteSlice,
    Form,
    None,
}

#[derive(Clone)]
pub enum TrustStorePem {
    Bytes(Vec<u8>),
    Path(std::path::PathBuf),
}