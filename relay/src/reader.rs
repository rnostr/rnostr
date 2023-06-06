use crate::message::*;
use actix::prelude::*;
use nostr_db::Db;
use std::sync::Arc;

pub struct Reader {
    pub db: Arc<Db>,
    pub addr: Recipient<ReadEventResult>,
    pub subscriptions: Vec<(usize, Subscription)>,
}

impl Reader {
    pub fn new(db: Arc<Db>, addr: Recipient<ReadEventResult>) -> Self {
        Self {
            db,
            addr,
            subscriptions: Vec::new(),
        }
    }
}

impl Actor for Reader {
    type Context = SyncContext<Self>;
    fn started(&mut self, _ctx: &mut Self::Context) {}
}

impl Handler<ReadEvent> for Reader {
    type Result = ();
    fn handle(&mut self, msg: ReadEvent, _: &mut Self::Context) {
        self.subscriptions.push((msg.id, msg.subscription));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temp_db_path;
    use anyhow::Result;

    #[derive(Default)]
    struct Receiver;
    impl Actor for Receiver {
        type Context = Context<Self>;
    }

    impl Handler<ReadEventResult> for Receiver {
        type Result = ();
        fn handle(&mut self, _msg: ReadEventResult, _ctx: &mut Self::Context) {}
    }

    #[actix_rt::test]
    async fn connect() -> Result<()> {
        let db = Arc::new(Db::open(temp_db_path("reader")?)?);

        let receiver = Receiver::start_default();
        let addr = receiver.recipient();

        let reader = SyncArbiter::start(3, move || Reader::new(Arc::clone(&db), addr.clone()));

        for i in 0..10 {
            reader
                .send(ReadEvent {
                    id: i,
                    subscription: Subscription::default(),
                })
                .await?;
        }

        Ok(())
    }
}
