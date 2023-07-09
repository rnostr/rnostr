pub mod auth;
pub use auth::Auth;

#[cfg(feature = "metrics")]
pub mod metrics;
#[cfg(feature = "metrics")]
pub use crate::metrics::Metrics;

#[cfg(feature = "rate_limiter")]
pub mod rate_limiter;
#[cfg(feature = "rate_limiter")]
pub use rate_limiter::Ratelimiter;

#[cfg(feature = "count")]
pub mod count;
#[cfg(feature = "count")]
pub use count::Count;

#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "search")]
pub use search::Search;

#[cfg(test)]
pub fn temp_data_path(p: &str) -> anyhow::Result<tempfile::TempDir> {
    Ok(tempfile::Builder::new()
        .prefix(&format!("nostr-relay-test-db-{}", p))
        .tempdir()?)
}

#[cfg(test)]
pub fn create_test_app(db_path: &str) -> anyhow::Result<nostr_relay::App> {
    Ok(nostr_relay::App::create(
        None,
        false,
        None,
        Some(temp_data_path(db_path)?),
    )?)
}
