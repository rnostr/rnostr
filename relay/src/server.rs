use crate::message::*;
use actix::prelude::*;
use std::collections::HashMap;

/// Server
#[derive(Debug, Default)]
pub struct Server {
    id: usize,
    sessions: HashMap<usize, Recipient<OutgoingMessage>>,
}

/// Make actor from `Server`
impl Actor for Server {
    /// We are going to use simple Context, we just need ability to communicate
    /// with other actors.
    type Context = Context<Self>;
}

/// Handler for Connect message.
///
/// Register new session and assign unique id to this session
impl Handler<Connect> for Server {
    type Result = usize;
    fn handle(&mut self, msg: Connect, _: &mut Context<Self>) -> Self::Result {
        if self.id == usize::MAX {
            self.id = 0;
        }
        self.id += 1;
        self.sessions.insert(self.id, msg.addr);
        // send id back
        self.id
    }
}

/// Handler for Disconnect message.
impl Handler<Disconnect> for Server {
    type Result = ();

    fn handle(&mut self, msg: Disconnect, _: &mut Context<Self>) {
        // remove address
        self.sessions.remove(&msg.id);
    }
}

/// Handler for Message message.
impl Handler<ClientMessage> for Server {
    type Result = ();

    fn handle(&mut self, _msg: ClientMessage, _: &mut Context<Self>) {}
}
