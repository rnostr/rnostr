use crate::{message::*, setting::SettingWrapper, Reader, Subscriber, Writer};
use actix::prelude::*;
use nostr_db::Db;
use std::{collections::HashMap, sync::Arc};

/// Server
#[derive(Debug)]
pub struct Server {
    id: usize,
    writer: Addr<Writer>,
    reader: Addr<Reader>,
    subscriber: Addr<Subscriber>,
    sessions: HashMap<usize, Recipient<OutgoingMessage>>,
}

impl Server {
    pub fn create_with(db: Arc<Db>, setting: SettingWrapper) -> Addr<Server> {
        let r = setting.read();
        let num = if r.thread.reader == 0 {
            num_cpus::get()
        } else {
            r.thread.reader
        };
        drop(r);

        Server::create(|ctx| {
            let writer = Writer::new(Arc::clone(&db), ctx.address().recipient()).start();
            let subscriber = Subscriber::new(ctx.address().recipient()).start();
            let addr = ctx.address().recipient();
            let reader =
                SyncArbiter::start(num, move || Reader::new(Arc::clone(&db), addr.clone()));

            Server {
                id: 0,
                writer,
                reader,
                subscriber,
                sessions: HashMap::new(),
            }
        })
    }

    fn send_to_client(&self, id: usize, msg: OutgoingMessage) {
        if let Some(addr) = self.sessions.get(&id) {
            addr.do_send(msg);
        }
    }
}

/// Make actor from `Server`
impl Actor for Server {
    /// We are going to use simple Context, we just need ability to communicate
    /// with other actors.
    type Context = Context<Self>;
    fn started(&mut self, _ctx: &mut Self::Context) {
        // ctx.set_mailbox_capacity(1);
    }
}

/// Handler for Connect message.
///
/// Register new session and assign unique id to this session
impl Handler<Connect> for Server {
    type Result = usize;
    fn handle(&mut self, msg: Connect, _ctx: &mut Self::Context) -> Self::Result {
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

    fn handle(&mut self, msg: Disconnect, _: &mut Self::Context) {
        // remove address
        self.sessions.remove(&msg.id);
    }
}

/// Handler for Message message.
impl Handler<ClientMessage> for Server {
    type Result = ();
    fn handle(&mut self, msg: ClientMessage, _: &mut Self::Context) {
        match msg.msg {
            IncomingMessage::Event { event } => {
                self.writer.do_send(WriteEvent { id: msg.id, event })
            }
            IncomingMessage::Close { id } => self.subscriber.do_send(Unsubscribe {
                id: msg.id,
                sub_id: Some(id),
            }),
            IncomingMessage::Req(subscription) => {
                self.reader.do_send(ReadEvent {
                    id: msg.id,
                    subscription: subscription.clone(),
                });
                self.subscriber.do_send(Subscribe {
                    id: msg.id,
                    subscription,
                })
            }
            IncomingMessage::Unknown => {
                self.send_to_client(msg.id, OutgoingMessage::notice("Unsupported message"));
            }
        }
    }
}

impl Handler<WriteEventResult> for Server {
    type Result = ();
    fn handle(&mut self, _msg: WriteEventResult, _: &mut Self::Context) {}
}

impl Handler<ReadEventResult> for Server {
    type Result = ();
    fn handle(&mut self, _msg: ReadEventResult, _: &mut Self::Context) {}
}

impl Handler<SubscribeResult> for Server {
    type Result = ();
    fn handle(&mut self, _msg: SubscribeResult, _: &mut Self::Context) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temp_db_path;
    use crate::Setting;
    use anyhow::Result;

    #[derive(Default)]
    struct Receiver;
    impl Actor for Receiver {
        type Context = Context<Self>;
    }

    impl Handler<OutgoingMessage> for Receiver {
        type Result = ();
        fn handle(&mut self, _msg: OutgoingMessage, _ctx: &mut Self::Context) {}
    }

    #[actix_rt::test]
    async fn connect() -> Result<()> {
        let db = Arc::new(Db::open(temp_db_path("")?)?);
        let server = Server::create_with(db, Setting::default_wrapper());
        let receiver = Receiver::start_default();

        let id = server
            .send(Connect {
                addr: receiver.recipient(),
            })
            .await?;
        assert_eq!(id, 1);
        Ok(())
    }
}
