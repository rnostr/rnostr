#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Notify(#[from] notify::Error),
    #[error(transparent)]
    Prometheus(#[from] prometheus::Error),
    #[error("error: {0}")]
    Message(String),
}

pub type Result<T, E = Error> = core::result::Result<T, E>;

mod app;
pub mod message;
mod metrics;
mod server;
mod session;
mod setting;

pub use {app::*, metrics::Metrics, server::Server, server::*, session::Session, setting::Setting};
