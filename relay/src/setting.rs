use crate::Result;
use config::{Config, File};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{
    any::{Any, TypeId},
    hash::{BuildHasherDefault, Hasher},
};
use tracing::{error, info};

pub const CARGO_PKG_VERSION: Option<&'static str> = option_env!("CARGO_PKG_VERSION");

fn default_version() -> String {
    CARGO_PKG_VERSION.map(ToOwned::to_owned).unwrap_or_default()
}

fn default_nips() -> Vec<u32> {
    vec![1, 2, 4, 9, 11, 12, 15, 16, 20, 22, 26, 33, 40]
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Information {
    pub name: String,
    pub description: String,
    pub pubkey: Option<String>,
    pub contact: Option<String>,
    pub software: String,
    #[serde(skip_deserializing, default = "default_version")]
    pub version: String,
    #[serde(skip_deserializing, default = "default_nips")]
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Db {
    pub path: PathBuf,
}

impl Default for Db {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./data"),
        }
    }
}

/// number of threads config
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Thread {
    /// number of http server threads
    pub http: usize,
    /// number of read event threads
    pub reader: usize,
}

impl Default for Thread {
    fn default() -> Self {
        Self { reader: 0, http: 0 }
    }
}

/// network config
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Network {
    /// server bind host
    pub host: String,
    /// server bind port
    pub port: u16,
    /// heartbeat timeout (default 120 seconds, must bigger than heartbeat interval)
    /// How long before lack of client response causes a timeout
    pub heartbeat_timeout: u64,

    /// heartbeat interval
    /// How often heartbeat pings are sent
    pub heartbeat_interval: u64,

    pub real_ip_header: Option<Vec<String>>,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7707,
            heartbeat_interval: 60,
            heartbeat_timeout: 120,
            real_ip_header: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
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

/// A hasher for `TypeId`s that takes advantage of its known characteristics.
///
/// Author of `anymap` crate has done research on the topic:
/// https://github.com/chris-morgan/anymap/blob/2e9a5704/src/lib.rs#L599
#[derive(Debug, Default)]
struct NoOpHasher(u64);

impl Hasher for NoOpHasher {
    fn write(&mut self, _bytes: &[u8]) {
        unimplemented!("This NoOpHasher can only handle u64s")
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Setting {
    pub information: Information,
    pub db: Db,
    pub thread: Thread,
    pub network: Network,
    pub limitation: Limitation,

    /// flatten extensions setting to json::Value
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,

    /// extensions setting object
    #[serde(skip)]
    extensions: HashMap<TypeId, Box<dyn Any + Send + Sync>, BuildHasherDefault<NoOpHasher>>,
}

pub type SettingWrapper = Arc<RwLock<Setting>>;

impl Setting {
    /// add supported nips
    pub fn add_nip(&mut self, nip: u32) {
        if self.information.supported_nips.contains(&nip) {
            self.information.supported_nips.push(nip);
            self.information.supported_nips.sort();
        }
    }

    /// get extension setting as json from extra
    pub fn get_extra_json(&self, key: &str) -> Option<String> {
        self.extra
            .get(key)
            .map(|h| serde_json::to_string(h).ok())
            .flatten()
    }

    /// Parse extension setting from extra json string. see [`crate::extensions::Metrics`]
    pub fn parse_extension<T: DeserializeOwned + Default>(&self, key: &str) -> T {
        self.get_extra_json(key)
            .map(|s| serde_json::from_str::<T>(&s).ok())
            .flatten()
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

    pub fn default_wrapper() -> SettingWrapper {
        Arc::new(RwLock::new(Self::default()))
    }

    /// information json
    pub fn render_information(&self) -> Result<String> {
        let info = &self.information;
        Ok(serde_json::to_string_pretty(&json!({
            "name": info.name,
            "description": info.description,
            "pubkey": info.pubkey,
            "contact": info.contact,
            "software": info.software,
            "version": info.version,
            "supported_nips": info.supported_nips,
            "limitation": &self.limitation,
        }))?)
    }

    pub fn read_wrapper<P: AsRef<Path>>(file: P) -> Result<SettingWrapper> {
        Ok(Arc::new(RwLock::new(Self::read(file)?)))
    }

    pub fn read<P: AsRef<Path>>(file: P) -> Result<Self> {
        let def = Self::default();

        let builder = Config::builder();
        let config = builder
            // use defaults
            .add_source(Config::try_from(&def)?)
            // override with file contents
            .add_source(File::with_name(file.as_ref().to_str().unwrap()))
            .build()?;

        let setting: Setting = config.try_deserialize()?;
        Ok(setting)
    }

    pub fn watch<P: AsRef<Path>, F: Fn(&SettingWrapper) + Send + 'static>(
        file: P,
        f: F,
    ) -> Result<(SettingWrapper, RecommendedWatcher)> {
        let setting = Self::read(&file)?;
        let setting = Arc::new(RwLock::new(setting));
        let c_file = file.as_ref().to_path_buf();
        let c_setting = Arc::clone(&setting);

        let mut watcher =
        // To make sure that the config lives as long as the function
        // we need to move the ownership of the config inside the function
        // To learn more about move please read [Using move Closures with Threads](https://doc.rust-lang.org/book/ch16-01-threads.html?highlight=move#using-move-closures-with-threads)
        RecommendedWatcher::new(move |result: Result<Event, notify::Error>| {
            match result {
                Ok(event) => {
                    if event.kind.is_modify() {
                        match Self::read(&c_file) {
                            Ok(new_setting) => {
                                info!("Reload config success {:?}", c_file);
                                {
                                  let mut w = c_setting.write();
                                  *w = new_setting;
                                }
                                f(&c_setting);
                            }
                            Err(e) => {
                                error!(error = e.to_string(), "failed to reload config {:?}", c_file);
                            }
                        }
                    }
                },
                Err(e) => {
                    error!(error = e.to_string(), "failed to watch file {:?}", c_file);
                },
            }
        }, notify::Config::default())?;

        watcher.watch(file.as_ref(), RecursiveMode::NonRecursive)?;

        Ok((setting, watcher))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::{fs, thread::sleep, time::Duration};
    use tempfile::Builder;

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

        let setting = Setting::read(&file)?;
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
        let setting = Setting::read(&file)?;
        assert_eq!(setting.information.name, "nostr".to_string());
        Ok(())
    }

    #[test]
    fn watch() -> Result<()> {
        let file = Builder::new()
            .prefix("nostr-relay-config-test-watch")
            .suffix(".toml")
            .tempfile()?;

        let (setting, _watcher) = Setting::watch(&file, |_s| {})?;
        {
            let r = setting.read();
            assert_eq!(r.information.name, "");
            assert!(r.information.supported_nips.contains(&1));
        }

        fs::write(
            &file,
            r#"[information]
    name = "nostr"
    "#,
        )?;
        sleep(Duration::from_millis(100));
        // println!("read {:?} {:?}", setting.read(), file);
        {
            let r = setting.read();
            assert_eq!(r.information.name, "nostr");
            assert!(r.information.supported_nips.contains(&1));
        }
        Ok(())
    }
}
