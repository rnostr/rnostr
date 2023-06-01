#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error("error: {0}")]
    Message(String),
}

mod message;

pub use message::*;