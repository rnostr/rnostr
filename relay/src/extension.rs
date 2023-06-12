use crate::{
    message::{ClientMessage, OutgoingMessage},
    Session,
};

pub enum ExtensionMessageResult {
    /// Continue run the next extension message method, the server takes over finally.
    Continue(ClientMessage),
    /// Stop run the next, send outgoing message to client.
    Stop(OutgoingMessage),
    /// Stop run the next, does not send any messages to the client.
    Ignore,
}

/// Extension for user session
pub trait Extension: Send + Sync {
    /// Execute after a user connect
    #[allow(unused_variables)]
    fn connected(&self, session: &mut Session, ctx: &mut <Session as actix::Actor>::Context) {}

    /// Execute when connection lost
    #[allow(unused_variables)]
    fn disconnected(&self, session: &mut Session, ctx: &mut <Session as actix::Actor>::Context) {}

    /// Execute when message incoming
    #[allow(unused_variables)]
    fn message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        ExtensionMessageResult::Continue(msg)
    }
}

/// extensions
#[derive(Default)]
pub struct Extensions {
    list: Vec<Box<dyn Extension>>,
}

impl Extensions {
    pub fn add<E: Extension + 'static>(&mut self, ext: E) {
        self.list.push(Box::new(ext));
    }

    pub fn call_connected(
        &self,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) {
        for ext in &self.list {
            ext.connected(session, ctx);
        }
    }

    pub fn call_disconnected(
        &self,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) {
        for ext in &self.list {
            ext.disconnected(session, ctx);
        }
    }

    pub fn call_message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        let mut msg = msg;
        for ext in &self.list {
            match ext.message(msg, session, ctx) {
                ExtensionMessageResult::Continue(m) => {
                    msg = m;
                }
                ExtensionMessageResult::Stop(o) => {
                    return ExtensionMessageResult::Stop(o);
                }
                ExtensionMessageResult::Ignore => {
                    return ExtensionMessageResult::Ignore;
                }
            };
        }
        ExtensionMessageResult::Continue(msg)
    }
}
