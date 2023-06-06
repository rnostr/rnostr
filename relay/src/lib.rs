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
mod reader;
mod server;
mod session;
mod setting;
mod subscriber;
mod writer;

pub use {
    app::*, reader::Reader, server::Server, server::*, session::Session, setting::Setting,
    subscriber::Subscriber, writer::Writer,
};

#[cfg(test)]
pub fn temp_db_path(p: &str) -> anyhow::Result<tempfile::TempDir> {
    Ok(tempfile::Builder::new()
        .prefix(&format!("nostr-relay-test-db-{}", p))
        .tempdir()?)
}
#[cfg(test)]
use lazy_static::lazy_static;
#[cfg(test)]
lazy_static! {
    pub static ref PROMETHEUS_HANDLE: metrics_exporter_prometheus::PrometheusHandle =
        create_prometheus_handle();
}
