use std::ffi::NulError;

pub mod lmdb;
pub mod scanner;

#[derive(thiserror::Error, Debug, Clone)]
pub enum Error {
    #[error(transparent)]
    CString(#[from] NulError),
    #[error("error: {0}")]
    Message(String),
    #[error("Lmdb error: {0}")]
    Lmdb(String),
}
