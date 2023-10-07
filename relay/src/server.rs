use crate::{message::*, setting::SettingWrapper, Reader, Subscriber, Writer};
use actix::prelude::*;
use nostr_db::{CheckEventResult, Db};
use std::{collections::HashMap, sync::Arc};
use tracing::info;

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
            let subscriber = Subscriber::new(ctx.address().recipient(), setting.clone()).start();
            let addr = ctx.address().recipient();
            info!("starting {} reader workers", num);
            let reader = SyncArbiter::start(num, move || {
                Reader::new(Arc::clone(&db), addr.clone(), setting.clone())
            });

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
    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.set_mailbox_capacity(10000);
        info!("Actor server started");
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

        // clear subscriptions
        self.subscriber.do_send(Unsubscribe {
            id: msg.id,
            sub_id: None,
        });
    }
}

/// Handler for Message message.
impl Handler<ClientMessage> for Server {
    type Result = ();
    fn handle(&mut self, msg: ClientMessage, ctx: &mut Self::Context) {
        match msg.msg {
            IncomingMessage::Event(event) => {
                // save all event
                // save ephemeral for check duplicate, disconnection recovery, will be deleted
                self.writer.do_send(WriteEvent { id: msg.id, event })
            }
            IncomingMessage::Close(id) => self.subscriber.do_send(Unsubscribe {
                id: msg.id,
                sub_id: Some(id),
            }),
            IncomingMessage::Req(subscription) => {
                let session_id = msg.id;
                let read_event = ReadEvent {
                    id: msg.id,
                    subscription: subscription.clone(),
                };
                self.subscriber
                    .send(Subscribe {
                        id: msg.id,
                        subscription,
                    })
                    .into_actor(self)
                    .then(move |res, act, _ctx| {
                        match res {
                            Ok(res) => match res {
                                Subscribed::Ok => {
                                    act.reader.do_send(read_event);
                                }
                                Subscribed::Overlimit => {
                                    act.send_to_client(
                                        session_id,
                                        OutgoingMessage::notice(
                                            "Number of subscriptions exceeds limit",
                                        ),
                                    );
                                }
                                Subscribed::InvalidIdLength => {
                                    act.send_to_client(
                                        session_id,
                                        OutgoingMessage::notice("Subscription id should be non-empty string of max length 64 chars"),
                                    );
                                }
                            },
                            Err(_err) => {
                                act.send_to_client(
                                    session_id,
                                    OutgoingMessage::notice("Something is wrong"),
                                );
                            }
                        }
                        fut::ready(())
                    })
                    .wait(ctx);
            }
            _ => {
                self.send_to_client(msg.id, OutgoingMessage::notice("Unsupported message"));
            }
        }
    }
}

impl Handler<WriteEventResult> for Server {
    type Result = ();
    fn handle(&mut self, msg: WriteEventResult, _: &mut Self::Context) {
        match msg {
            WriteEventResult::Write { id, event, result } => {
                let event_id = event.id_str();
                let out_msg = match &result {
                    CheckEventResult::Ok(_num) => OutgoingMessage::ok(&event_id, true, ""),
                    CheckEventResult::Duplicate => {
                        OutgoingMessage::ok(&event_id, true, "duplicate: event exists")
                    }
                    CheckEventResult::Invald(msg) => {
                        OutgoingMessage::ok(&event_id, false, &format!("invalid: {}", msg))
                    }
                    CheckEventResult::Deleted => {
                        OutgoingMessage::ok(&event_id, false, "deleted: user requested deletion")
                    }
                    CheckEventResult::ReplaceIgnored => {
                        OutgoingMessage::ok(&event_id, false, "replaced: have newer event")
                    }
                };
                self.send_to_client(id, out_msg);
                // dispatch event to subscriber
                if let CheckEventResult::Ok(_num) = result {
                    self.subscriber.do_send(Dispatch { id, event });
                }
            }
            WriteEventResult::Message { id, event: _, msg } => {
                self.send_to_client(id, msg);
            }
        }
    }
}

impl Handler<ReadEventResult> for Server {
    type Result = ();
    fn handle(&mut self, msg: ReadEventResult, _: &mut Self::Context) {
        self.send_to_client(msg.id, msg.msg);
    }
}

impl Handler<SubscribeResult> for Server {
    type Result = ();
    fn handle(&mut self, msg: SubscribeResult, _: &mut Self::Context) {
        self.send_to_client(msg.id, msg.msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{temp_data_path, Setting};
    use actix_rt::time::sleep;
    use anyhow::Result;
    use parking_lot::RwLock;
    use std::time::Duration;

    #[derive(Default)]
    struct Receiver(Arc<RwLock<Vec<OutgoingMessage>>>);
    impl Actor for Receiver {
        type Context = Context<Self>;
    }

    impl Handler<OutgoingMessage> for Receiver {
        type Result = ();
        fn handle(&mut self, msg: OutgoingMessage, _ctx: &mut Self::Context) {
            self.0.write().push(msg);
        }
    }

    #[actix_rt::test]
    async fn message() -> Result<()> {
        let db = Arc::new(Db::open(temp_data_path("server")?)?);
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
        let ephemeral_note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d78",
            "kind": 20000,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"]]
          }
        "#;

        let receiver = Receiver::default();
        let messages = receiver.0.clone();
        let receiver = receiver.start();
        let addr = receiver.recipient();

        let server = Server::create_with(db, Setting::default().into());

        let id = server.send(Connect { addr }).await?;
        assert_eq!(id, 1);

        // Unsupported
        {
            let text = r#"["UNKNOWN"]"#.to_owned();
            let msg = serde_json::from_str::<IncomingMessage>(&text)?;
            let client_msg = ClientMessage { id, text, msg };
            server.send(client_msg).await?;
            sleep(Duration::from_millis(50)).await;
            {
                let mut w = messages.write();
                assert_eq!(w.len(), 1);
                assert!(w.get(0).unwrap().0.contains("Unsupported"));
                w.clear();
            }
        }

        // Subscribe
        {
            let text = r#"["REQ", "1", {}]"#.to_owned();
            let msg = serde_json::from_str::<IncomingMessage>(&text)?;
            let client_msg = ClientMessage { id, text, msg };
            server.send(client_msg).await?;
            sleep(Duration::from_millis(50)).await;
            {
                let mut w = messages.write();
                assert_eq!(w.len(), 1);
                assert!(w.get(0).unwrap().0.contains("EOSE"));
                w.clear();
            }

            // write
            let text = format!(r#"["EVENT", {}]"#, note);
            let msg = serde_json::from_str::<IncomingMessage>(&text)?;
            let client_msg = ClientMessage { id, text, msg };
            server.send(client_msg.clone()).await?;
            sleep(Duration::from_millis(200)).await;
            {
                let mut w = messages.write();
                assert_eq!(w.len(), 2);
                assert!(w.get(0).unwrap().0.contains("OK"));
                // subscription message
                assert!(w.get(1).unwrap().0.contains("EVENT"));
                w.clear();
            }
            // repeat write
            server.send(client_msg.clone()).await?;
            sleep(Duration::from_millis(200)).await;
            {
                let mut w = messages.write();
                assert_eq!(w.len(), 1);
                assert!(w.get(0).unwrap().0.contains("OK"));
                // No subscription message because the message is duplicated
                w.clear();
            }

            // ephemeral event
            {
                let text = format!(r#"["EVENT", {}]"#, ephemeral_note);
                let msg = serde_json::from_str::<IncomingMessage>(&text)?;
                let client_msg = ClientMessage { id, text, msg };
                server.send(client_msg.clone()).await?;
                sleep(Duration::from_millis(200)).await;
                {
                    let mut w = messages.write();
                    assert_eq!(w.len(), 2);
                    assert!(w.get(0).unwrap().0.contains("OK"));
                    // subscription message
                    assert!(w.get(1).unwrap().0.contains("EVENT"));
                    w.clear();
                }
                // repeat
                server.send(client_msg.clone()).await?;
                sleep(Duration::from_millis(200)).await;
                {
                    let mut w = messages.write();
                    assert_eq!(w.len(), 1);
                    assert!(w.get(0).unwrap().0.contains("OK"));
                    // No subscription message because the message is duplicated
                    w.clear();
                }
            }

            // unsubscribe

            let text = r#"["CLOSE", "1"]"#.to_owned();
            let msg = serde_json::from_str::<IncomingMessage>(&text)?;
            let client_msg = ClientMessage { id, text, msg };
            server.send(client_msg).await?;
            sleep(Duration::from_millis(50)).await;
            {
                let mut w = messages.write();
                // assert_eq!(w.len(), 1);
                // assert!(w.get(0).unwrap().0.contains("EOSE"));
                w.clear();
            }
        }

        // get
        {
            let text = r#"["REQ", "1", {}]"#.to_owned();
            let msg = serde_json::from_str::<IncomingMessage>(&text)?;
            let client_msg = ClientMessage { id, text, msg };
            server.send(client_msg).await?;
            sleep(Duration::from_millis(50)).await;
            {
                let mut w = messages.write();
                assert_eq!(w.len(), 3);
                assert!(w.get(0).unwrap().0.contains("EVENT"));
                assert!(w.get(1).unwrap().0.contains("EVENT"));
                assert!(w.get(2).unwrap().0.contains("EOSE"));
                w.clear();
            }
        }

        Ok(())
    }
}
