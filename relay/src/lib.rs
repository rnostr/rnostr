#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error("error: {0}")]
    Message(String),
}

mod app;
pub mod message;
mod server;
mod session;

pub use {
    app::create_app, app::route, app::start_app, server::Server, server::*, session::Session,
};
