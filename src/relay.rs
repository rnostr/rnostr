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
    info!("Start relay server");

    let app_data = App::create(Some(config), watch, Some("RNOSTR".to_owned()), None)?;
    register_extensions(app_data)?.web_server()?.await?;
    info!("Relay server shutdown");

    Ok(())
}

fn register_extensions(app_data: App) -> nostr_relay::Result<App> {
    let db = app_data.db.clone();
    app_data
        .add_extension(nostr_extensions::Metrics::new())?
        .add_extension(nostr_extensions::Auth::new())?
        .add_extension(nostr_extensions::Ratelimiter::new())?
        .add_extension(nostr_extensions::Count::new(db.clone()))?
        .add_extension(nostr_extensions::EventPolicy::new())?
        .add_extension(nostr_extensions::Search::new())?
        .add_extension(nostr_extensions::Negentropies::new(db))?
        .web_server()?
        .await?;
    info!("Relay server shutdown");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{error::PayloadError, web};
    use actix_web_actors::ws;
    use nostr_relay::{
        db::Event,
        message::{ClientMessage, IncomingMessage},
        ExtensionMessageResult, Session,
    };
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct CheckRuntimeOnDrop(Arc<AtomicBool>);

    #[actix_rt::test]
    async fn event_policy_runs_after_auth_and_rate_limits() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let config = directory.path().join("relay.toml");
        std::fs::write(
            &config,
            r#"
[thread]
reader = 1
[auth]
enabled = true
[auth.event]
ip_blacklist = ["192.0.2.1"]
[rate_limiter]
enabled = true
[[rate_limiter.event]]
period = "1h"
limit = 1
[event_policy]
enabled = true
allowed_kinds = [1]
"#,
        )?;
        let app = App::create(Some(config.as_path()), false, None, Some(directory.path()))?;
        let app = web::Data::new(register_extensions(app)?);
        let stream = futures_util::stream::empty::<Result<web::Bytes, PayloadError>>();
        let _context = ws::WebsocketContext::with_factory(stream, move |ctx| {
            let mut reject = |ip: &str, tags| {
                let mut session = Session::new(ip.into(), app.clone());
                let event =
                    Event::new([1; 32], [1; 32], 1, 1059, tags, String::new(), [0; 64]).unwrap();
                let msg = ClientMessage::new(0, String::new(), IncomingMessage::Event(event));
                let ExtensionMessageResult::Stop(response) =
                    app.extensions.read().call_message(msg, &mut session, ctx)
                else {
                    panic!("expected an EVENT rejection");
                };
                let (_, _, accepted, reason): (String, String, bool, String) =
                    serde_json::from_str(&response.0).unwrap();
                assert!(!accepted);
                reason
            };
            for _ in 0..2 {
                assert_eq!(reject("192.0.2.1", vec![]), "restricted: ip in blacklist");
            }
            assert!(reject("192.0.2.2", vec![vec!["-".into()]]).starts_with("auth-required:"));
            // Policy-rejected writes still consume the applicable rate-limit quota.
            assert!(reject("192.0.2.2", vec![]).starts_with("blocked:"));
            assert!(reject("192.0.2.2", vec![]).starts_with("rate-limited:"));
            Session::new("192.0.2.2".into(), app)
        });
        Ok(())
    }

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
