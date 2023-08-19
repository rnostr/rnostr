#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Kv(#[from] nostr_kv::Error),
    #[error(transparent)]
    ConvertU64(#[from] std::array::TryFromSliceError),
    #[error(transparent)]
    Secp256k1(#[from] secp256k1::Error),
    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("deserialization: {0}")]
    Deserialization(String),
    #[error("serialization: {0}")]
    Serialization(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("invalid length")]
    InvalidLength,
    #[error("message: {0}")]
    Message(String),
    #[error("Scan timeout")]
    ScanTimeout,
    #[error("The database schema has been modified. Please run export first, move the old database file, then import and start the program.
      Find the rnostr command at https://github.com/rnostr/rnostr#commands
      rnostr export data/events > events.json
      mv data/events data/old_events
      rnostr import data/events events.json
    ")]
    VersionMismatch,
}
