

#[derive(Clone)]
pub enum HttpVersion {
    V1_0,
    V1_1,
}

#[derive(Clone, Debug)]
pub enum TrustStorePem {
    Bytes(Vec<u8>),
    Path(std::path::PathBuf),
}