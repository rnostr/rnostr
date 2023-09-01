use crate::{message::*, Result};
use actix::prelude::*;
use metrics::{histogram, increment_counter};
use nostr_db::{now, CheckEventResult, Db};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, error, info};

/// Single-threaded write events, delete expired events
/// Batch write can improve tps

const WRITE_INTERVAL_MS: u64 = 100;
const DEL_INTERVAL_SECONDS: u64 = 60;

pub struct Writer {
    pub db: Arc<Db>,
    pub addr: Recipient<WriteEventResult>,
    pub events: Vec<WriteEvent>,
    pub write_interval_ms: u64,
    pub del_interval_seconds: u64,
}

impl Writer {
    pub fn new(db: Arc<Db>, addr: Recipient<WriteEventResult>) -> Self {
        Self {
            db,
            addr,
            events: Vec::new(),
            write_interval_ms: WRITE_INTERVAL_MS,
            del_interval_seconds: DEL_INTERVAL_SECONDS,
        }
    }

    pub fn write(&mut self) -> Result<()> {
        if !self.events.is_empty() {
            debug!("write events: {:?}", self.events);
            let start = Instant::now();
            let mut writer = self.db.writer()?;
            while let Some(event) = self.events.pop() {
                match self.db.put(&mut writer, &event.event) {
                    Ok(result) => {
                        if let CheckEventResult::Ok(_num) = result {
                            increment_counter!("nostr_relay_new_event");
                        }
                        self.addr.do_send(WriteEventResult::Write {
                            id: event.id,
                            event: event.event,
                            result,
                        });
                    }
                    Err(err) => {
                        error!(error = err.to_string(), "write event error");
                        let eid = event.event.id_str();
                        self.addr.do_send(WriteEventResult::Message {
                            id: event.id,
                            event: event.event,
                            msg: OutgoingMessage::ok(&eid, false, "write event error"),
                        });
                    }
                }
            }
            self.db.commit(writer)?;
            histogram!("nostr_relay_db_write", start.elapsed());
        }
        Ok(())
    }

    pub fn do_write(&mut self) {
        if let Err(err) = self.write() {
            error!(error = err.to_string(), "write events error");
        }
    }

    pub fn del_expired(&self) -> Result<()> {
        let reader = self.db.reader()?;
        let iter = self
            .db
            .iter_expiration::<Vec<u8>, _>(&reader, Some(now()))?;
        let mut ids = vec![];
        for id in iter {
            let id = id?;
            ids.push(id);
        }
        self.db.batch_del(ids)?;
        Ok(())
    }

    pub fn do_del_expired(&self) {
        if let Err(err) = self.del_expired() {
            error!(error = err.to_string(), "delete expired events error");
        }
    }
}

impl Actor for Writer {
    type Context = Context<Self>;
    fn started(&mut self, ctx: &mut Self::Context) {
        info!("Actor writer started");
        // save event every 100ms
        ctx.run_interval(
            Duration::from_millis(self.write_interval_ms),
            |act, _ctx| {
                act.do_write();
            },
        );
        // delete expired events every minute
        ctx.run_interval(
            Duration::from_secs(self.del_interval_seconds),
            |act, _ctx| {
                act.do_del_expired();
            },
        );
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        info!("Actor writer stopped");
        // save event when stopped
        self.do_write();
    }
}

impl Handler<WriteEvent> for Writer {
    type Result = ();
    fn handle(&mut self, msg: WriteEvent, _: &mut Self::Context) {
        self.events.push(msg);
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, time::Duration};

    use super::*;
    use crate::temp_data_path;
    use actix_rt::time::sleep;
    use anyhow::Result;
    use nostr_db::Event;
    use parking_lot::RwLock;

    #[derive(Default)]
    struct Receiver(Arc<RwLock<Vec<WriteEventResult>>>);
    impl Actor for Receiver {
        type Context = Context<Self>;
    }

    impl Handler<WriteEventResult> for Receiver {
        type Result = ();
        fn handle(&mut self, msg: WriteEventResult, _ctx: &mut Self::Context) {
            self.0.write().push(msg);
        }
    }

    #[actix_rt::test]
    async fn write() -> Result<()> {
        let db = Arc::new(Db::open(temp_data_path("writer")?)?);
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

        let receiver = Receiver::default();
        let messages = receiver.0.clone();
        let receiver = receiver.start();
        let addr = receiver.recipient();

        let writer = Writer::new(Arc::clone(&db), addr.clone()).start();

        for i in 0..4 {
            writer
                .send(WriteEvent {
                    id: i,
                    event: event.clone(),
                })
                .await?;
        }

        sleep(Duration::from_millis(WRITE_INTERVAL_MS * 3)).await;
        let r = messages.read();
        assert_eq!(r.len(), 4);
        Ok(())
    }
}
