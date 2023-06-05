use crate::message::*;
use actix::prelude::*;
use nostr_db::Event;

pub struct Subscriber {
    pub addr: Recipient<WriteEventResult>,
    pub events: Vec<(usize, Event)>,
}

impl Subscriber {
    pub fn new(addr: Recipient<WriteEventResult>) -> Self {
        Self {
            addr,
            events: Vec::new(),
        }
    }
}

impl Actor for Subscriber {
    type Context = Context<Self>;
    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.set_mailbox_capacity(10000);
    }
}

impl Handler<Subscribe> for Subscriber {
    type Result = ();
    fn handle(&mut self, _msg: Subscribe, _: &mut Self::Context) {}
}

impl Handler<Unsubscribe> for Subscriber {
    type Result = ();
    fn handle(&mut self, _msg: Unsubscribe, _: &mut Self::Context) {}
}

impl Handler<Dispatch> for Subscriber {
    type Result = ();
    fn handle(&mut self, _msg: Dispatch, _: &mut Self::Context) {}
}
