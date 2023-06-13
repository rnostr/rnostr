//! Nostr event database

mod db;
mod error;
mod event;
mod filter;
mod key;

pub use {
    db::CheckEventResult, db::Db, db::Iter, error::Error, event::now, event::ArchivedEventIndex,
    event::Event, event::EventIndex, event::FromEventData, filter::Filter, filter::TagList,
};

pub use nostr_kv as kv;

/// Stats of query
#[derive(Debug, Clone)]
pub struct Stats {
    pub scan_index: u64,
    pub get_data: u64,
    pub get_index: u64,
}
