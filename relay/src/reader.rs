use crate::message::*;
use actix::prelude::*;
use nostr_db::{Db, Event};
use std::sync::Arc;

pub struct Reader {
    pub db: Arc<Db>,
    pub addr: Recipient<ReadEventResult>,
    pub subscriptions: Vec<(usize, Event)>,
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
    fn handle(&mut self, _msg: ReadEvent, _: &mut Self::Context) {}
}
