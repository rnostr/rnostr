use crate::message::*;
use actix::prelude::*;
use nostr_db::{Db, Event};
use std::sync::Arc;

pub struct Writer {
    pub db: Arc<Db>,
    pub addr: Recipient<WriteEventResult>,
    pub events: Vec<(usize, Event)>,
}

impl Writer {
    pub fn new(db: Arc<Db>, addr: Recipient<WriteEventResult>) -> Self {
        Self {
            db,
            addr,
            events: Vec::new(),
        }
    }
}

impl Actor for Writer {
    type Context = Context<Self>;
    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.set_mailbox_capacity(10000);
    }
}

impl Handler<WriteEvent> for Writer {
    type Result = ();
    fn handle(&mut self, _msg: WriteEvent, _: &mut Self::Context) {}
}
