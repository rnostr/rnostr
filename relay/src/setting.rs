use crate::Result;
use config::{Config, File};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info};

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Information {
    pub name: Option<String>,
    pub description: Option<String>,
    pub pubkey: Option<String>,
    pub contact: Option<String>,
    // supported_nips, software, version
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Session {
    /// heartbeat interval
    pub heartbeat_interval: u64,
    /// heartbeat timeout
    pub heartbeat_timeout: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            heartbeat_interval: 5,
            heartbeat_timeout: 10,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Setting {
    pub information: Information,
    pub session: Session,
}

impl Setting {
    pub fn read<P: AsRef<Path>>(file: P) -> Result<Self> {
        let def = Self::default();

        let builder = Config::builder();
        let config = builder
            // use defaults
            .add_source(Config::try_from(&def)?)
            // override with file contents
            .add_source(File::with_name(file.as_ref().to_str().unwrap()))
            .build()?;

        let config: Setting = config.try_deserialize()?;
        Ok(config)
    }

    pub fn watch<P: AsRef<Path>>(file: P) -> Result<(Arc<RwLock<Self>>, RecommendedWatcher)> {
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
                                info!("Reload setting success {:?}", c_file);
                                let mut w = c_setting.write();
                                *w = new_setting;
                            }
                            Err(e) => {
                                error!(error = e.to_string(), "failed to reload setting {:?}", c_file);
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
        let file = Builder::new()
            .prefix("nostr-relay-config-test-read")
            .suffix(".toml")
            .rand_bytes(0)
            .tempfile()?;

        let setting = Setting::read(&file)?;
        assert_eq!(setting.information.name, None);
        fs::write(
            &file,
            r#"[information]
        name = "nostr"
        "#,
        )?;
        let setting = Setting::read(&file)?;
        assert_eq!(setting.information.name, Some("nostr".to_string()));
        Ok(())
    }

    #[test]
    fn watch() -> Result<()> {
        let file = Builder::new()
            .prefix("nostr-relay-config-test-watch")
            .suffix(".toml")
            .rand_bytes(0)
            .tempfile()?;

        let (setting, _watcher) = Setting::watch(&file)?;
        assert_eq!(setting.read().information.name, None);
        fs::write(
            &file,
            r#"[information]
    name = "nostr"
    "#,
        )?;
        sleep(Duration::from_millis(100));
        // println!("read {:?} {:?}", setting.read(), file);
        assert_eq!(setting.read().information.name, Some("nostr".to_string()));
        Ok(())
    }
}
