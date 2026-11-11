use crate::Error;
use crate::{duration::NonZeroDuration, hash::NoOpHasherDefault, Result};
use config::{Config, Environment, File, FileFormat, Source};
use notify::{
    event::{ModifyKind, RenameMode},
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tracing::{debug, error, info};

pub const CARGO_PKG_VERSION: Option<&'static str> = option_env!("CARGO_PKG_VERSION");

fn default_version() -> String {
    CARGO_PKG_VERSION.map(ToOwned::to_owned).unwrap_or_default()
}

fn default_nips() -> Vec<u32> {
    vec![1, 2, 4, 9, 11, 12, 15, 16, 20, 22, 25, 26, 28, 33, 40, 70]
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct Information {
    pub name: String,
    pub description: String,
    pub pubkey: Option<String>,
    pub contact: Option<String>,
    pub software: String,
    #[serde(skip_deserializing)]
    pub version: String,
    #[serde(skip_deserializing)]
    pub supported_nips: Vec<u32>,
}

impl Default for Information {
    fn default() -> Self {
        Self {
            name: Default::default(),
            description: Default::default(),
            pubkey: Default::default(),
            contact: Default::default(),
            software: Default::default(),
            version: default_version(),
            supported_nips: default_nips(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct Data {
    pub path: PathBuf,

    /// Query filter timeout time
    pub db_query_timeout: Option<NonZeroDuration>,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./data"),
            db_query_timeout: None,
        }
    }
}

/// number of threads config
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Thread {
    /// number of http server threads
    pub http: usize,
    /// number of read event threads
    pub reader: usize,
}

/// network config
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct Network {
    /// server bind host
    pub host: String,
    /// server bind port
    pub port: u16,
    /// heartbeat timeout (default 120 seconds, must bigger than heartbeat interval)
    /// How long before lack of client response causes a timeout
    pub heartbeat_timeout: NonZeroDuration,

    /// heartbeat interval
    /// How often heartbeat pings are sent
    pub heartbeat_interval: NonZeroDuration,

    pub real_ip_header: Option<String>,

    /// redirect to other site when user access the http index page
    pub index_redirect_to: Option<String>,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            heartbeat_interval: Duration::from_secs(60).try_into().unwrap(),
            heartbeat_timeout: Duration::from_secs(120).try_into().unwrap(),
            real_ip_header: None,
            index_redirect_to: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct Limitation {
    /// this is the maximum number of bytes for incoming JSON. default 512K
    pub max_message_length: usize,
    /// total number of subscriptions that may be active on a single websocket connection to this relay. default 20
    pub max_subscriptions: usize,
    /// maximum number of filter values in each subscription. default 10
    pub max_filters: usize,
    /// the relay server will clamp each filter's limit value to this number. This means the client won't be able to get more than this number of events from a single subscription filter. default 300
    pub max_limit: u64,
    /// maximum length of subscription id as a string. default 100
    pub max_subid_length: usize,
    /// for authors and ids filters which are to match against a hex prefix, you must provide at least this many hex digits in the prefix. default 10
    pub min_prefix: usize,
    /// in any event, this is the maximum number of elements in the tags list. default 5000
    pub max_event_tags: usize,
    /// Events older than this will be rejected. default 3 years, 0 ignore
    pub max_event_time_older_than_now: u64,
    /// Events newer than this will be rejected. default 15 minutes, 0 ignore
    pub max_event_time_newer_than_now: u64,
}

impl Default for Limitation {
    fn default() -> Self {
        Self {
            max_message_length: 524288,
            max_subscriptions: 20,
            max_filters: 10,
            max_limit: 300,
            max_subid_length: 100,
            min_prefix: 10,
            max_event_tags: 5000,
            max_event_time_older_than_now: 94608000,
            max_event_time_newer_than_now: 900,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Setting {
    pub information: Information,
    pub data: Data,
    pub thread: Thread,
    pub network: Network,
    pub limitation: Limitation,

    /// flatten extensions setting to json::Value
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,

    /// extensions setting object
    #[serde(skip)]
    extensions: HashMap<TypeId, Box<dyn Any + Send + Sync>, NoOpHasherDefault>,

    /// nip-11 extension information
    #[serde(skip)]
    ext_information: HashMap<String, Value>,

    /// nip-11 extension limitation
    #[serde(skip)]
    ext_limitation: HashMap<String, Value>,
}

impl PartialEq for Setting {
    fn eq(&self, other: &Self) -> bool {
        self.information == other.information
            && self.data == other.data
            && self.thread == other.thread
            && self.network == other.network
            && self.limitation == other.limitation
            && self.extra == other.extra
    }
}

#[derive(Debug, Clone)]
pub struct SettingWrapper {
    inner: Arc<RwLock<Setting>>,
    watcher: Option<Arc<RecommendedWatcher>>,
}

impl Deref for SettingWrapper {
    type Target = Arc<RwLock<Setting>>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl From<Setting> for SettingWrapper {
    fn from(setting: Setting) -> Self {
        Self {
            inner: Arc::new(RwLock::new(setting)),
            watcher: None,
        }
    }
}

impl SettingWrapper {
    /// Reload settings from file, with optional environment overrides.
    /// Files that parse to no settings are ignored and return `Ok(())`.
    pub fn reload<P: AsRef<Path>>(&self, file: P, env_prefix: Option<String>) -> Result<()> {
        self.reload_file(file.as_ref(), env_prefix.as_deref())
            .map(|_| ())
    }

    fn reload_file(&self, file: &Path, env_prefix: Option<&str>) -> Result<bool> {
        let config = Config::builder().add_source(File::from(file)).build()?;
        // Check the file before env overrides, then apply this same cached parse.
        if config.collect()?.is_empty() {
            debug!(file = %file.display(), "skipping reload: no settings in file");
            return Ok(false);
        }
        let setting = Setting::from_config(config, env_prefix)?;
        *self.write() = setting;
        Ok(true)
    }

    fn reload_event(&self, event: &Event, file: &Path, env_prefix: Option<&str>) -> Result<bool> {
        if !config_changed(event, file) {
            return Ok(false);
        }
        self.reload_file(file, env_prefix)
    }

    /// config from file and watch file update then reload
    pub fn watch<P: AsRef<Path>, F: Fn(&SettingWrapper) + Send + 'static>(
        file: P,
        env_prefix: Option<String>,
        f: F,
    ) -> Result<Self> {
        let mut setting: SettingWrapper = Setting::read(&file, env_prefix.clone())?.into();
        let c_setting = setting.clone();

        // let file = current_dir()?.join(file.as_ref());
        // symbolic links
        let file = fs::canonicalize(file.as_ref())?;
        let c_file = file.clone();

        // support vim editor. watch dir
        // https://docs.rs/notify/latest/notify/#editor-behaviour
        // https://github.com/notify-rs/notify/issues/113#issuecomment-281836995

        let dir = file
            .parent()
            .ok_or_else(|| Error::Message("failed to get config dir".to_owned()))?;

        let mut watcher = RecommendedWatcher::new(
            move |result: Result<Event, notify::Error>| match result {
                Ok(event) => match c_setting.reload_event(&event, &c_file, env_prefix.as_deref()) {
                    Ok(true) => {
                        info!("Reload config success {:?}", c_file);
                        info!("{:?}", c_setting.read());
                        f(&c_setting);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        error!(
                            error = e.to_string(),
                            "failed to reload config {:?}", c_file
                        );
                    }
                },
                Err(e) => {
                    error!(error = e.to_string(), "failed to watch file {:?}", c_file);
                }
            },
            notify::Config::default(),
        )?;

        watcher.watch(dir, RecursiveMode::NonRecursive)?;
        // save watcher
        setting.watcher = Some(Arc::new(watcher));

        Ok(setting)
    }
}

fn config_changed(event: &Event, file: &Path) -> bool {
    let targets_file = match event.kind {
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => false,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            // On Linux, notify reports the destination in both To and Both.
            // Ignore Both to avoid reloading twice.
            !cfg!(target_os = "linux")
                && event
                    .paths
                    .last()
                    .is_some_and(|destination| destination == file)
        }
        EventKind::Create(_)
        | EventKind::Modify(ModifyKind::Any | ModifyKind::Data(_) | ModifyKind::Name(_)) => {
            event.paths.iter().any(|path| path == file)
        }
        _ => false,
    };
    targets_file && file.is_file()
}

impl Setting {
    /// add supported nips for nip-11 information
    pub fn add_nip(&mut self, nip: u32) {
        if !self.information.supported_nips.contains(&nip) {
            self.information.supported_nips.push(nip);
            self.information.supported_nips.sort();
        }
    }

    /// add nip-11 extension information
    pub fn add_information(&mut self, key: String, value: Value) {
        self.ext_information.insert(key, value);
    }

    /// add nip-11 extension limitation
    pub fn add_limitation(&mut self, key: String, value: Value) {
        self.ext_limitation.insert(key, value);
    }

    /// Parse extension setting.
    pub fn parse_extension<T: DeserializeOwned + Default>(&self, key: &str) -> T {
        self.extra
            .get(key)
            .and_then(|v| {
                let r = serde_json::from_value::<T>(v.clone());
                if let Err(err) = &r {
                    error!(error = err.to_string(), "failed to parse {:?} setting", key);
                }
                r.ok()
            })
            .unwrap_or_default()
    }

    /// save extension setting
    pub fn set_extension<T: Send + Sync + 'static>(&mut self, val: T) {
        self.extensions.insert(TypeId::of::<T>(), Box::new(val));
    }

    /// get extension setting
    pub fn get_extension<T: 'static>(&self) -> Option<&T> {
        self.extensions
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref())
    }

    /// nip-11 information json
    pub fn render_information(&self) -> Result<String> {
        let info = &self.information;
        let mut val = json!({
            "name": info.name,
            "description": info.description,
            "pubkey": info.pubkey,
            "contact": info.contact,
            "software": info.software,
            "version": info.version,
            "supported_nips": info.supported_nips,
            "limitation": &self.limitation,
        });
        self.ext_limitation.iter().for_each(|(k, v)| {
            val["limitation"][k] = v.clone();
        });
        self.ext_information.iter().for_each(|(k, v)| {
            val[k] = v.clone();
        });
        Ok(serde_json::to_string_pretty(&val)?)
    }

    /// read config from file and env
    pub fn read<P: AsRef<Path>>(file: P, env_prefix: Option<String>) -> Result<Self> {
        let config = Config::builder()
            .add_source(File::from(file.as_ref()))
            .build()?;
        Self::from_config(config, env_prefix.as_deref())
    }

    fn from_config(config: Config, env_prefix: Option<&str>) -> Result<Self> {
        let mut config = Config::builder().add_source(config);
        if let Some(prefix) = env_prefix {
            config = config.add_source(Self::env_source(prefix));
        }

        let config = config.build()?;
        let mut setting: Setting = config.try_deserialize()?;
        setting.correct();
        Ok(setting)
    }

    fn env_source(prefix: &str) -> Environment {
        Environment::with_prefix(prefix)
            .try_parsing(true)
            .prefix_separator("_")
            .separator("__")
        // .list_separator(" ")
        // .with_list_parse_key("")
    }

    /// read config from env
    pub fn from_env(env_prefix: String) -> Result<Self> {
        let mut config = Config::builder();
        config = config.add_source(Self::env_source(&env_prefix));
        let config = config.build()?;
        let mut setting: Setting = config.try_deserialize()?;
        setting.correct();
        Ok(setting)
    }

    /// config from str
    pub fn from_str(s: &str, format: FileFormat) -> Result<Self> {
        let builder = Config::builder();
        let config = builder.add_source(File::from_str(s, format)).build()?;
        let mut setting: Setting = config.try_deserialize()?;
        setting.correct();
        Ok(setting)
    }

    fn correct(&mut self) {
        if self.network.heartbeat_timeout <= self.network.heartbeat_interval {
            error!("network heartbeat_timeout must bigger than heartbeat_interval, use defaults");
            self.network.heartbeat_interval = Duration::from_secs(60).try_into().unwrap();
            self.network.heartbeat_timeout = Duration::from_secs(120).try_into().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use config::FileFormat;
    use notify::event::{CreateKind, DataChange};
    use std::{
        fs,
        time::{Duration, Instant},
    };
    use tempfile::Builder;

    #[test]
    fn der() -> Result<()> {
        let json = r#"{
            "network": {"port": 1},
            "information": {"name": "test"},
            "data": {},
            "thread": {"http": 1},
            "limitation": {}
        }"#;

        let mut def = Setting::default();
        def.network.port = 1;
        def.information.name = "test".to_owned();
        def.thread.http = 1;

        let s2 = serde_json::from_str::<Setting>(json)?;
        let s1: Setting = Setting::from_str(json, FileFormat::Json)?;

        assert_eq!(def, s1);
        assert_eq!(def, s2);

        Ok(())
    }

    #[test]
    fn render() -> Result<()> {
        let mut def = Setting::default();
        def.add_nip(1234567);
        def.add_limitation("payment_required".to_owned(), json!(true));
        def.add_information("payments_url".to_owned(), json!("https://payments"));
        let info = def.render_information()?;
        let val: Value = serde_json::from_str(&info)?;
        // println!("{:?}", info);
        assert!(val["supported_nips"]
            .as_array()
            .unwrap()
            .contains(&Value::Number(serde_json::Number::from(1234567))));
        assert_eq!(val["payments_url"], json!("https://payments"));
        assert_eq!(val["limitation"]["payment_required"], json!(true));
        Ok(())
    }

    #[test]
    fn read() -> Result<()> {
        let setting = Setting::default();
        assert_eq!(setting.information.name, "");
        assert!(setting.information.supported_nips.contains(&1));

        let file = Builder::new()
            .prefix("nostr-relay-config-test-read")
            .suffix(".toml")
            .rand_bytes(0)
            .tempfile()?;

        let setting = Setting::read(&file, None)?;
        assert_eq!(setting.information.name, "");
        assert!(setting.information.supported_nips.contains(&1));
        fs::write(
            &file,
            r#"
        [information]
        name = "nostr"
        [network]
        host = "127.0.0.1"
        "#,
        )?;

        temp_env::with_vars(
            [
                ("NOSTR_information.description", Some("test")),
                ("NOSTR_information__contact", Some("test")),
                ("NOSTR_INFORMATION__PUBKEY", Some("test")),
                ("NOSTR_NETWORK__PORT", Some("1")),
            ],
            || {
                let setting = Setting::read(&file, Some("NOSTR".to_owned())).unwrap();
                assert_eq!(setting.information.name, "nostr".to_string());
                assert_eq!(setting.information.description, "test".to_string());
                assert_eq!(setting.information.contact, Some("test".to_string()));
                assert_eq!(setting.information.pubkey, Some("test".to_string()));
                assert_eq!(setting.network.port, 1);
            },
        );
        Ok(())
    }

    #[test]
    fn config_events_match_destinations_and_existing_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let config = dir.path().join("relay.toml");
        let other = dir.path().join("other.toml");
        fs::write(&config, "")?;
        let rename = |mode| EventKind::Modify(ModifyKind::Name(mode));
        for (kind, paths, expected) in [
            (
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                vec![config.clone()],
                true,
            ),
            (rename(RenameMode::From), vec![config.clone()], false),
            (rename(RenameMode::To), vec![config.clone()], true),
            (
                rename(RenameMode::Both),
                vec![config.clone(), other.clone()],
                false,
            ),
            (
                rename(RenameMode::Both),
                vec![other.clone(), config.clone()],
                !cfg!(target_os = "linux"),
            ),
            (rename(RenameMode::Any), vec![config.clone()], true),
            (EventKind::Modify(ModifyKind::Any), vec![other], false),
            (
                EventKind::Create(CreateKind::File),
                vec![config.clone()],
                true,
            ),
            (
                EventKind::Create(CreateKind::Any),
                vec![config.clone()],
                true,
            ),
            (
                EventKind::Create(CreateKind::File),
                vec![dir.path().join("other.toml")],
                false,
            ),
        ] {
            let mut event = Event::new(kind);
            event.paths = paths;
            assert_eq!(config_changed(&event, &config), expected, "{event:?}");
        }
        fs::remove_file(&config)?;
        assert!(!config_changed(
            &Event::new(EventKind::Modify(ModifyKind::Any)).add_path(config.clone()),
            &config
        ));
        Ok(())
    }

    #[test]
    fn watcher_sequences_skip_empty_files_and_load_created_replacements() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("relay.toml");
        let content = |name| format!("[information]\nname = '{name}'\n[network]\nport = 9001\n");
        fs::write(&path, content("initial"))?;
        let setting: SettingWrapper = Setting::read(&path, None)?.into();
        let create = Event::new(EventKind::Create(CreateKind::File)).add_path(path.clone());
        let modify =
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any))).add_path(path.clone());
        for recreate in [false, true] {
            let previous = setting.read().information.name.clone();
            if recreate {
                fs::remove_file(&path)?;
                assert!(!setting.reload_event(&modify, &path, None)?);
            }
            for content in ["", " \n\t ", "# file is still being written\n"] {
                fs::write(&path, content)?;
                for event in [&create, &modify] {
                    assert!(!setting.reload_event(event, &path, None)?);
                    assert_eq!(setting.read().information.name, previous);
                    assert_eq!(setting.read().network.port, 9001);
                }
            }
            fs::write(&path, "[network]\nport = ")?;
            assert!(setting.reload_event(&create, &path, None).is_err());
            assert!(setting.reload_event(&modify, &path, None).is_err());
            assert_eq!(setting.read().information.name, previous);
            assert_eq!(setting.read().network.port, 9001);
            fs::write(&path, content("complete"))?;
            assert!(setting.reload_event(&create, &path, None)?);
            assert_eq!(setting.read().information.name, "complete");
        }
        fs::write(&path, content("atomic"))?;
        for (mode, expected) in [
            (RenameMode::To, true),
            (RenameMode::Both, !cfg!(target_os = "linux")),
        ] {
            let mut event = Event::new(EventKind::Modify(ModifyKind::Name(mode)));
            if mode == RenameMode::Both {
                event = event.add_path(dir.path().join("replacement.toml"));
            }
            event = event.add_path(path.clone());
            assert_eq!(setting.reload_event(&event, &path, None)?, expected);
        }
        Ok(())
    }

    #[test]
    fn reload_preserves_json_format_and_environment_overrides() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("relay.json");
        fs::write(&path, r#"{"information":{"name":"initial"}}"#)?;
        let setting: SettingWrapper = Setting::read(&path, None)?.into();
        temp_env::with_vars(
            [("RELOAD_JSON_TEST_NETWORK__PORT", Some("9003"))],
            || -> Result<()> {
                fs::write(&path, "{}")?;
                let event = Event::new(EventKind::Create(notify::event::CreateKind::File))
                    .add_path(path.clone());
                assert!(!setting.reload_event(&event, &path, Some("RELOAD_JSON_TEST"))?);
                assert_eq!(setting.read().information.name, "initial");
                fs::write(
                    &path,
                    r#"{"information":{"name":"updated"},"network":{"port":9002}}"#,
                )?;
                setting.reload(&path, Some("RELOAD_JSON_TEST".into()))?;
                assert_eq!(setting.read().information.name, "updated");
                assert_eq!(setting.read().network.port, 9003);
                Ok(())
            },
        )?;
        Ok(())
    }

    #[test]
    fn watch() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("relay.toml");
        let content = |name: &str| format!("[information]\nname = '{name}'\n");
        fs::write(&path, content("initial"))?;
        let (changed, changes) = std::sync::mpsc::channel();
        let setting = SettingWrapper::watch(&path, None, move |setting| {
            let _ = changed.send(setting.read().information.name.clone());
        })?;
        for (name, atomic) in [("inplace", false), ("atomic", true)] {
            if atomic {
                let replacement = dir.path().join("replacement.toml");
                fs::write(&replacement, content(name))?;
                fs::rename(replacement, &path)?;
            } else {
                fs::write(&path, content(name))?;
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while changes.recv_timeout(deadline.saturating_duration_since(Instant::now()))? != name
            {
            }
            assert_eq!(setting.read().information.name, name);
        }
        Ok(())
    }
}
