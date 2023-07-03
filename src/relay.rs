use crate::Result;
use clap::Parser;
use nostr_relay::{extensions, App};
use std::path::PathBuf;
use tracing::info;

/// Start relay options
#[derive(Debug, Clone, Parser)]
pub struct RelayOpts {
    /// Nostr relay config path
    #[arg(short = 'c', value_name = "PATH", default_value = "./nostr.toml")]
    pub config: PathBuf,

    /// Auto reload when config changed
    #[arg(long, value_name = "BOOL")]
    pub watch: bool,
}

pub fn relay(config: &PathBuf, watch: bool) -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Start relay server");

    actix_rt::System::new().block_on(async {
        let app_data = App::create(Some(config), watch, Some("NOSTR".to_owned()), None).unwrap();
        app_data
            .add_extension(extensions::Metrics::new())
            .add_extension(extensions::Auth::new())
            .add_extension(extensions::Ratelimiter::new())
            .web_server()
            .unwrap()
            .await
            .unwrap();
        info!("Relay server shutdown");
    });

    Ok(())
}
