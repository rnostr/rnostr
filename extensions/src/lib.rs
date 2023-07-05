pub mod auth;
pub mod metrics;
pub mod rate_limiter;

pub use {self::metrics::Metrics, auth::Auth, rate_limiter::Ratelimiter};

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
