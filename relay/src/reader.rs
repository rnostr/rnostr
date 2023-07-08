use crate::{message::*, setting::SettingWrapper, Result};
use actix::prelude::*;
use metrics::histogram;
use nostr_db::Db;
use std::{sync::Arc, time::Instant};

/// Requst by filter
/// Concurrent read events from db
pub struct Reader {
    pub db: Arc<Db>,
    pub addr: Recipient<ReadEventResult>,
    pub setting: SettingWrapper,
}

impl Reader {
    pub fn new(db: Arc<Db>, addr: Recipient<ReadEventResult>, setting: SettingWrapper) -> Self {
        Self { db, addr, setting }
    }

    pub fn read(&self, msg: &ReadEvent) -> Result<()> {
        let reader = self.db.reader()?;
        let timeout = self.setting.read().data.db_query_timeout;
        for filter in &msg.subscription.filters {
            let start = Instant::now();
            let mut iter = self.db.iter::<String, _>(&reader, filter)?;
            if let Some(time) = timeout {
                iter.scan_time(time.into(), 2000);
            }
            for event in iter {
                let event = event?;
                self.addr.do_send(ReadEventResult {
                    id: msg.id,
                    sub_id: msg.subscription.id.clone(),
                    msg: OutgoingMessage::event(&msg.subscription.id, &event),
                });
            }
            histogram!("nostr_relay_db_get", start.elapsed());
        }
        self.addr.do_send(ReadEventResult {
            id: msg.id,
            sub_id: msg.subscription.id.clone(),
            msg: OutgoingMessage::eose(&msg.subscription.id),
        });

        Ok(())
    }
}

impl Actor for Reader {
    type Context = SyncContext<Self>;
    fn started(&mut self, _ctx: &mut Self::Context) {}
}

impl Handler<ReadEvent> for Reader {
    type Result = ();
    fn handle(&mut self, msg: ReadEvent, _: &mut Self::Context) {
        if let Err(err) = self.read(&msg) {
            self.addr.do_send(ReadEventResult {
                id: msg.id,
                sub_id: msg.subscription.id,
                msg: OutgoingMessage::notice(&format!("get event error: {}", err)),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{temp_data_path, Setting};
    use actix_rt::time::sleep;
    use anyhow::Result;
    use nostr_db::{Event, Filter};
    use parking_lot::RwLock;
    use std::{str::FromStr, time::Duration};

    #[derive(Default)]
    struct Receiver(Arc<RwLock<Vec<ReadEventResult>>>);
    impl Actor for Receiver {
        type Context = Context<Self>;
    }

    impl Handler<ReadEventResult> for Receiver {
        type Result = ();
        fn handle(&mut self, msg: ReadEventResult, _ctx: &mut Self::Context) {
            self.0.write().push(msg);
        }
    }

    #[actix_rt::test]
    async fn read() -> Result<()> {
        let db = Arc::new(Db::open(temp_data_path("reader")?)?);
        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"]]
          }
        "#;
        let event = Event::from_str(note)?;
        db.batch_put(vec![event])?;

        let receiver = Receiver::default();
        let messages = receiver.0.clone();
        let receiver = receiver.start();
        let addr = receiver.recipient();

        let reader = SyncArbiter::start(3, move || {
            Reader::new(Arc::clone(&db), addr.clone(), Setting::default().into())
        });

        for i in 0..4 {
            reader
                .send(ReadEvent {
                    id: i,
                    subscription: Subscription {
                        id: i.to_string(),
                        filters: vec![Filter {
                            ..Default::default()
                        }],
                    },
                })
                .await?;
        }

        sleep(Duration::from_millis(100)).await;
        let r = messages.read();
        assert_eq!(r.len(), 8);
        Ok(())
    }
}
