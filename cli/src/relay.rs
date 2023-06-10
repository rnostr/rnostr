use crate::Result;
use clap::Parser;
use nostr_relay::{create_app, start_app};
use std::path::PathBuf;
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
    Ok(())
}
