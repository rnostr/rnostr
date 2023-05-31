#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Kv(#[from] nostr_kv::Error),
    #[error(transparent)]
    ConvertU64(#[from] std::array::TryFromSliceError),
    #[error("Json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Io: {0}")]
    Io(#[from] std::io::Error),
    #[error("Hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("Deserialization: {0}")]
    Deserialization(String),
    #[error("Serialization: {0}")]
    Serialization(String),
    #[error("Invald: {0}")]
    Invald(String),
    #[error("Invald length")]
    InvaldLength,
    #[error("message: {0}")]
    Message(String),
}
