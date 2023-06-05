#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Notify(#[from] notify::Error),
    #[error(transparent)]
    Prometheus(#[from] metrics_exporter_prometheus::BuildError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("error: {0}")]
    Message(String),
}

impl actix_web::ResponseError for Error {}

pub type Result<T, E = Error> = core::result::Result<T, E>;

mod app;
pub mod message;
mod server;
mod writer;
mod reader;
mod subscriber;
mod session;
mod setting;

pub use {app::*, server::Server, reader::Reader, writer::Writer, subscriber::Subscriber, server::*, session::Session, setting::Setting};
