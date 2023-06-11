use crate::Result;
use clap::Parser;
use nostr_relay::{create_prometheus_handle, start_app, AppData, Setting};
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

    let r = if watch {
        let r = Setting::watch(config)?;
        (r.0, Some(r.1))
    } else {
        (Setting::read_wrapper(config)?, None)
    };
    let prometheus_handle = create_prometheus_handle();

    actix_rt::System::new().block_on(async {
        let app_data = AppData::create::<PathBuf>(r.0, None, prometheus_handle).unwrap();
        start_app(app_data).unwrap().await.unwrap();
    });

    Ok(())
}
