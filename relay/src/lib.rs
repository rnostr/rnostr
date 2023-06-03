#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Notify(#[from] notify::Error),
    #[error("error: {0}")]
    Message(String),
}

pub type Result<T, E = Error> = core::result::Result<T, E>;

mod app;
pub mod message;
mod server;
mod session;
mod setting;

pub use {app::*, server::Server, server::*, session::Session, setting::Setting};
