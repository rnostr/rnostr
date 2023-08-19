use crate::Result;
use clap::Parser;
use nostr_relay::App;
use std::path::PathBuf;
use tracing::info;

/// Start relay options
#[derive(Debug, Clone, Parser)]
pub struct RelayOpts {
    /// Nostr relay config path
    #[arg(
        short = 'c',
        value_name = "PATH",
        default_value = "./config/rnostr.toml"
    )]
    pub config: PathBuf,

    /// Auto reload when config changed
    #[arg(long, value_name = "BOOL")]
    pub watch: bool,
}

#[actix_rt::main]
pub async fn relay(config: &PathBuf, watch: bool) -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Start relay server");

    // actix_rt::System::new().block_on(async {
    // });

    let app_data = App::create(Some(config), watch, Some("RNOSTR".to_owned()), None)?;
    let db = app_data.db.clone();
    app_data
        .add_extension(nostr_extensions::Metrics::new())
        .add_extension(nostr_extensions::Auth::new())
        .add_extension(nostr_extensions::Ratelimiter::new())
        .add_extension(nostr_extensions::Count::new(db))
        .add_extension(nostr_extensions::Search::new())
        .web_server()?
        .await?;
    info!("Relay server shutdown");

    Ok(())
}
