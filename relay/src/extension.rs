use crate::{
    message::{ClientMessage, OutgoingMessage},
    setting::SettingWrapper,
    Result, Session, Setting,
};
use actix_web::web::ServiceConfig;

pub enum ExtensionMessageResult {
    /// Continue run the next extension message method, the server takes over finally.
    Continue(ClientMessage),
    /// Stop run the next, send outgoing message to client.
    Stop(OutgoingMessage),
    /// Stop run the next, does not send any messages to the client.
    Ignore,
}

impl From<OutgoingMessage> for ExtensionMessageResult {
    fn from(value: OutgoingMessage) -> Self {
        Self::Stop(value)
    }
}

/// A prepared, infallible update to an extension's live state.
/// Must not panic: a panic can leave a reload partially applied.
pub type SettingUpdate<'a> = Box<dyn FnOnce() + 'a>;

/// Extension for user session
pub trait Extension: Send + Sync {
    fn name(&self) -> &'static str;

    /// Prepare configuration without changing live state. Commit changes in the returned update.
    #[allow(unused_variables)]
    fn setting<'a>(&'a mut self, setting: &mut Setting) -> Result<SettingUpdate<'a>> {
        Ok(Box::new(|| {}))
    }

    /// config actix web service
    #[allow(unused_variables)]
    fn config_web(&mut self, cfg: &mut ServiceConfig) {}

    /// Execute after a user connect
    #[allow(unused_variables)]
    fn connected(&self, session: &mut Session, ctx: &mut <Session as actix::Actor>::Context) {}

    /// Execute when connection lost
    #[allow(unused_variables)]
    fn disconnected(&self, session: &mut Session, ctx: &mut <Session as actix::Actor>::Context) {}

    /// Execute when message incoming
    #[allow(unused_variables)]
    fn message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        ExtensionMessageResult::Continue(msg)
    }
}

/// extensions
#[derive(Default)]
pub struct Extensions {
    list: Vec<Box<dyn Extension>>,
}

impl Extensions {
    pub fn add<E: Extension + 'static>(&mut self, ext: E) {
        self.list.push(Box::new(ext));
    }

    pub fn call_setting(&mut self, setting: &SettingWrapper, mut candidate: Setting) -> Result<()> {
        let updates = self
            .list
            .iter_mut()
            .map(|ext| ext.setting(&mut candidate))
            .collect::<Result<Vec<_>>>()?;
        *setting.write() = candidate;
        for update in updates {
            update();
        }
        Ok(())
    }

    pub fn call_config_web(&mut self, cfg: &mut ServiceConfig) {
        for ext in &mut self.list {
            ext.config_web(cfg);
        }
    }

    pub fn call_connected(
        &self,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) {
        for ext in &self.list {
            ext.connected(session, ctx);
        }
    }

    pub fn call_disconnected(
        &self,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) {
        for ext in &self.list {
            ext.disconnected(session, ctx);
        }
    }

    pub fn call_message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        let mut msg = msg;
        for ext in &self.list {
            match ext.message(msg, session, ctx) {
                ExtensionMessageResult::Continue(m) => {
                    msg = m;
                }
                ExtensionMessageResult::Stop(o) => {
                    return ExtensionMessageResult::Stop(o);
                }
                ExtensionMessageResult::Ignore => {
                    return ExtensionMessageResult::Ignore;
                }
            };
        }
        ExtensionMessageResult::Continue(msg)
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;
    use std::{
        fs,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc,
        },
        time::{Duration, Instant},
    };

    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields)]
    struct Policy {
        #[serde(deserialize_with = "crate::setting::deserialize_scalar")]
        value: usize,
    }

    struct Probe {
        name: &'static str,
        value: Arc<AtomicUsize>,
        notices: Option<mpsc::Sender<Option<usize>>>,
    }

    impl Probe {
        fn new(name: &'static str, value: Arc<AtomicUsize>) -> Self {
            Self {
                name,
                value,
                notices: None,
            }
        }
    }

    impl Extension for Probe {
        fn name(&self) -> &'static str {
            self.name
        }

        fn setting<'a>(&'a mut self, setting: &mut Setting) -> Result<SettingUpdate<'a>> {
            let policy: Policy = match setting.parse_extension(self.name()) {
                Ok(policy) => policy,
                Err(error) => {
                    if let Some(notices) = &self.notices {
                        let _ = notices.send(None);
                    }
                    return Err(error);
                }
            };
            if policy.value > 0 {
                setting.add_nip(99);
            }
            setting.set_extension(policy.value);
            Ok(Box::new(move || {
                self.value.store(policy.value, Ordering::Relaxed);
                if let Some(notices) = &self.notices {
                    let _ = notices.send(Some(policy.value));
                }
            }))
        }
    }

    #[actix_rt::test]
    async fn failed_registration_does_not_publish_preparation_side_effects() -> anyhow::Result<()> {
        struct Reject;
        impl Extension for Reject {
            fn name(&self) -> &'static str {
                "reject"
            }
            fn setting<'a>(&'a mut self, candidate: &mut Setting) -> Result<SettingUpdate<'a>> {
                candidate.information.name = "leaked".to_owned();
                candidate.add_nip(999);
                candidate.add_information("leaked".to_owned(), true.into());
                candidate.add_limitation("leaked".to_owned(), true.into());
                candidate.set_extension(99usize);
                Err(crate::Error::Str("rejected after preparation"))
            }
        }
        let app = crate::create_test_app("config-registration")?
            .add_extension(Probe::new("policy", Arc::new(AtomicUsize::new(0))))?;
        let setting = app.setting.clone();
        let before = serde_json::to_value(&*setting.read())?;
        let information = setting.read().render_information()?;
        assert!(app.add_extension(Reject).is_err());
        assert_eq!(serde_json::to_value(&*setting.read())?, before);
        assert_eq!(setting.read().render_information()?, information);
        assert_eq!(setting.read().get_extension::<usize>(), Some(&0));
        Ok(())
    }

    #[actix_rt::test]
    async fn manual_reload_reuses_the_startup_environment_prefix() -> anyhow::Result<()> {
        temp_env::with_var(
            "RELOAD_COMPAT_POLICY__VALUE",
            Some("7"),
            || -> anyhow::Result<()> {
                let directory = tempfile::tempdir()?;
                let file = directory.path().join("relay.toml");
                fs::write(&file, "[policy]\nvalue = 1\n")?;
                let value = Arc::new(AtomicUsize::new(0));
                let app = crate::App::create(
                    Some(&file),
                    false,
                    Some("RELOAD_COMPAT".to_owned()),
                    Some(&directory.path().to_path_buf()),
                )?
                .add_extension(Probe::new("policy", value.clone()))?;
                assert_eq!(value.load(Ordering::Relaxed), 7);
                fs::write(&file, "")?;
                assert!(app.reload(&file).is_err());
                assert_eq!(value.load(Ordering::Relaxed), 7);
                fs::write(&file, "[policy]\nvalue = 3\n")?;
                app.reload(&file)?;
                assert_eq!(value.load(Ordering::Relaxed), 7);
                temp_env::with_var("RELOAD_COMPAT_POLICY__VALUE", Some("9"), || {
                    app.reload(&file)
                })?;
                assert_eq!(value.load(Ordering::Relaxed), 9);
                Ok(())
            },
        )
    }

    #[actix_rt::test]
    async fn failed_reload_keeps_all_settings_and_extension_state() -> anyhow::Result<()> {
        let first = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(AtomicUsize::new(0));
        let app = crate::create_test_app("config-transaction")?;
        app.setting.write().extra =
            serde_json::from_value(json!({"first": {"value": 1}, "last": {"value": 2}}))?;
        let app = app
            .add_extension(Probe::new("first", first.clone()))?
            .add_extension(Probe::new("last", last.clone()))?;
        let before = serde_json::to_value(&*app.setting.read())?;
        let information = app.setting.read().render_information()?;
        let file = tempfile::Builder::new().suffix(".toml").tempfile()?;
        for invalid in [
            "",
            " \n\t ",
            "# file is still being written\n",
            "[information]\nname = \"new\"\n[network]\nport = \"bad\"",
            "[information]\nname = \"new\"\n[first]\nvalue = 8\n[last]\nvalue = \"bad\"",
        ] {
            fs::write(file.path(), invalid)?;
            assert!(app.reload(file.path()).is_err());
            assert_eq!(serde_json::to_value(&*app.setting.read())?, before);
            assert_eq!(app.setting.read().render_information()?, information);
            assert_eq!(app.setting.read().get_extension::<usize>(), Some(&2));
            assert_eq!(first.load(Ordering::Relaxed), 1);
            assert_eq!(last.load(Ordering::Relaxed), 2);
        }
        fs::write(
            file.path(),
            "[information]\nname = \"new\"\n[first]\nvalue = 8\n",
        )?;
        app.reload(file.path())?;
        assert_eq!(app.setting.read().information.name, "new");
        assert_eq!(app.setting.read().get_extension::<usize>(), Some(&0));
        assert_eq!(first.load(Ordering::Relaxed), 8);
        assert_eq!(last.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[actix_rt::test]
    async fn watched_reload_skips_empty_and_recovers_after_rejection() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("relay.toml");
        let replacement = directory.path().join("replacement.toml");
        fs::write(&file, "[policy]\nvalue = 1\n")?;
        let (notices, received) = mpsc::channel();
        let value = Arc::new(AtomicUsize::new(0));
        let app = crate::App::create(
            Some(&file),
            true,
            None,
            Some(&directory.path().to_path_buf()),
        )?
        .add_extension(Probe {
            name: "policy",
            value: value.clone(),
            notices: Some(notices),
        })?;
        assert_eq!(received.recv_timeout(Duration::from_secs(5))?, Some(1));
        assert_eq!(app.setting.read().information.name, "");
        assert!(app.setting.read().information.supported_nips.contains(&1));
        fs::write(&file, "")?;
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            match received.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                result => assert_eq!(result?, Some(1), "empty file changed the policy"),
            }
        }
        assert_eq!(value.load(Ordering::Relaxed), 1);
        assert_eq!(app.setting.read().get_extension::<usize>(), Some(&1));
        assert!(app.setting.read().information.supported_nips.contains(&99));
        for (text, expected, atomic) in [
            (
                "[information]\nname = \"rejected\"\n[policy]\nvalue = \"bad\"\n",
                None,
                true,
            ),
            (
                "[information]\nname = \"nostr\"\n[policy]\nvalue = 7\n",
                Some(7),
                false,
            ),
            (
                "[information]\nname = \"nostr\"\n[policy]\nvalue = 9\n",
                Some(9),
                true,
            ),
        ] {
            if atomic {
                fs::write(&replacement, text)?;
                fs::rename(&replacement, &file)?;
            } else {
                fs::write(&file, text)?;
            }
            loop {
                let applied = received.recv_timeout(Duration::from_secs(5))?;
                assert_ne!(applied, Some(0), "applied the default policy");
                if applied == expected {
                    break;
                }
            }
            assert_eq!(value.load(Ordering::Relaxed), expected.unwrap_or(1));
            assert_eq!(
                app.setting.read().get_extension::<usize>(),
                Some(&expected.unwrap_or(1))
            );
            assert_eq!(
                app.setting.read().information.name,
                if expected.is_some() { "nostr" } else { "" }
            );
            assert!(app.setting.read().information.supported_nips.contains(&1));
        }
        Ok(())
    }
}
