#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error("error: {0}")]
    Message(String),
}

mod app;
mod message;
mod server;
mod session;

pub use {app::create_app, app::route, app::start_app, message::*, server::*, session::Session};
