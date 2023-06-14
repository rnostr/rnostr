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
    #[error("invalid: {0}")]
    Invalid(String),
}

impl actix_web::ResponseError for Error {}

pub type Result<T, E = Error> = core::result::Result<T, E>;

mod app;
mod extension;
pub mod extensions;
pub mod message;
mod reader;
mod server;
mod session;
mod setting;
mod subscriber;
mod writer;

pub use {
    app::*, extension::*, reader::Reader, server::Server, server::*, session::Session,
    setting::Setting, subscriber::Subscriber, writer::Writer,
};

#[cfg(test)]
pub fn temp_db_path(p: &str) -> anyhow::Result<tempfile::TempDir> {
    Ok(tempfile::Builder::new()
        .prefix(&format!("nostr-relay-test-db-{}", p))
        .tempdir()?)
}

#[cfg(test)]
pub fn create_test_app(db_path: &str) -> anyhow::Result<App> {
    Ok(App::create(None, false, Some(temp_db_path(db_path)?))?)
}
