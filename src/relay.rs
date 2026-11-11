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

pub fn relay(config: &PathBuf, watch: bool) -> Result<()> {
    run_in_system(run_relay(config, watch))
}

fn run_in_system<F: std::future::Future>(future: F) -> F::Output {
    let system = actix_rt::System::new();
    let handle = system.runtime().tokio_runtime().handle().clone();
    let _entered = handle.enter();
    // Drop the system before the enter guard, including during unwinding.
    let system = system;
    system.block_on(future)
}

async fn run_relay(config: &PathBuf, watch: bool) -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Start relay server");

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct CheckRuntimeOnDrop(Arc<AtomicBool>);

    impl Drop for CheckRuntimeOnDrop {
        fn drop(&mut self) {
            let entered = std::panic::catch_unwind(|| {
                let _timer = actix_rt::time::sleep(std::time::Duration::from_secs(1));
            })
            .is_ok();
            self.0.store(entered, Ordering::Relaxed);
        }
    }

    #[test]
    fn startup_panic_preserves_the_original_error() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let config = directory.path().join("relay.toml");
        std::fs::write(&config, "[thread]\nreader = 1\n")?;
        let entered_during_cleanup = Arc::new(AtomicBool::new(false));
        let probe = CheckRuntimeOnDrop(entered_during_cleanup.clone());
        let result = std::panic::catch_unwind(|| {
            run_in_system(async {
                let _app = App::create(Some(config.as_path()), false, None, Some(directory.path()))
                    .unwrap();
                actix_rt::spawn(async move {
                    let _probe = probe;
                    std::future::pending::<()>().await;
                });
                panic!("startup panic");
            });
        });
        assert_eq!(
            result.unwrap_err().downcast_ref::<&str>(),
            Some(&"startup panic")
        );
        assert!(entered_during_cleanup.load(Ordering::Relaxed));
        Ok(())
    }
}
