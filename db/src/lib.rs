//! Nostr event database

mod db;
mod error;
mod event;
mod filter;
mod key;
pub use secp256k1;

pub use {
    db::CheckEventResult, db::Db, db::Iter, error::Error, event::now, event::ArchivedEventIndex,
    event::Event, event::EventIndex, event::FromEventData, filter::Filter, filter::SortList,
};

pub use nostr_kv as kv;

/// Stats of query
#[derive(Debug, Clone)]
pub struct Stats {
    pub scan_index: u64,
    pub get_data: u64,
    pub get_index: u64,
}

#[cfg(feature = "search")]
use charabia::Segment;

#[cfg(feature = "search")]
/// segment keywords by charabia
pub fn segment(content: &str) -> Vec<Vec<u8>> {
    let iter = content.segment_str();
    let mut words = iter
        .filter_map(|s| {
            let s = s.to_lowercase();
            let bytes = s.as_bytes();
            // limit size
            if bytes.len() < 255 {
                Some(bytes.to_vec())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    words.sort();
    words.dedup();
    words
}
