use crate::{hash::NoOpHasherDefault, message::*, App, Server};
use actix::prelude::*;
use actix_http::ws::Item;
use actix_web::web;
use actix_web_actors::ws;
use bytes::BytesMut;
use metrics::{decrement_gauge, increment_counter, increment_gauge};
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    time::{Duration, Instant},
};
use tracing::debug;
use ws::Message;

pub struct Session {
    ip: String,

    /// unique session id
    id: usize,

    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    hb: Instant,

    /// server
    server: Addr<Server>,

    /// heartbeat timeout
    /// How long before lack of client response causes a timeout
    heartbeat_timeout: Duration,

    /// heartbeat interval
    /// How often heartbeat pings are sent
    heartbeat_interval: Duration,

    pub app: web::Data<App>,

    /// Simple store for save extension data
    data: HashMap<TypeId, Box<dyn Any>, NoOpHasherDefault>,

    /// Buffer for constructing continuation messages
    cont: Option<BytesMut>,
}

impl Session {
    /// save extension data
    pub fn set<T: 'static>(&mut self, val: T) {
        self.data.insert(TypeId::of::<T>(), Box::new(val));
    }

    /// get extension data
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.data
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref())
    }

    /// Get session id
    pub fn id(&self) -> usize {
        self.id
    }

    /// Get ip
    pub fn ip(&self) -> &String {
        &self.ip
    }

    pub fn new(ip: String, app: web::Data<App>) -> Session {
        let setting = app.setting.read();
        let heartbeat_timeout = setting.network.heartbeat_timeout.into();
        let heartbeat_interval = setting.network.heartbeat_interval.into();
        drop(setting);
        Self {
            id: 0,
            ip,
            hb: Instant::now(),
            server: app.server.clone(),
            heartbeat_timeout,
            heartbeat_interval,
            app,
            data: HashMap::default(),
            cont: None,
        }
    }

    /// helper method that sends ping to client.
    /// also this method checks heartbeats from client
    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(self.heartbeat_interval, |act, ctx| {
            // check client heartbeats
            if Instant::now().duration_since(act.hb) > act.heartbeat_timeout {
                // heartbeat timed out
                // stop actor
                increment_counter!("nostr_relay_session_stop_total", "reason" => "heartbeat timeout");
                ctx.stop();
                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
    }

    fn handle_message(&mut self, text: String, ctx: &mut ws::WebsocketContext<Self>) {
        let msg = serde_json::from_str::<IncomingMessage>(&text);
        match msg {
            Ok(msg) => {
                if let Some(cmd) = msg.known_command() {
                    // only insert known command metrics
                    increment_counter!("nostr_relay_message_total", "command" => cmd);
                }

                let mut msg = ClientMessage {
                    id: self.id,
                    text,
                    msg,
                };
                {
                    let r = self.app.setting.read();
                    if let Err(err) = msg.validate(&r.limitation) {
                        if let IncomingMessage::Event(event) = &msg.msg {
                            ctx.text(OutgoingMessage::ok(
                                &event.id_str(),
                                false,
                                &err.to_string(),
                            ));
                        } else {
                            ctx.text(OutgoingMessage::notice(&err.to_string()));
                        }
                        return;
                    }
                }

                match self
                    .app
                    .clone()
                    .extensions
                    .read()
                    .call_message(msg, self, ctx)
                {
                    crate::ExtensionMessageResult::Continue(msg) => {
                        self.server.do_send(msg);
                    }
                    crate::ExtensionMessageResult::Stop(out) => {
                        ctx.text(out);
                    }
                    crate::ExtensionMessageResult::Ignore => {
                        // ignore
                    }
                };
            }
            Err(err) => {
                ctx.text(OutgoingMessage::notice(&format!("json error: {}", err)));
            }
        };
    }
}

/// Handle messages from server, we simply send it to peer websocket
impl Handler<OutgoingMessage> for Session {
    type Result = ();

    fn handle(&mut self, msg: OutgoingMessage, ctx: &mut Self::Context) {
        ctx.text(msg);
    }
}

impl Actor for Session {
    type Context = ws::WebsocketContext<Self>;

    /// Method is called on actor start. We start the heartbeat process here.
    fn started(&mut self, ctx: &mut Self::Context) {
        increment_counter!("nostr_relay_session_total");
        increment_gauge!("nostr_relay_session", 1.0);

        // we'll start heartbeat process on session start.
        self.hb(ctx);
        // register self in server.
        let addr = ctx.address();
        self.server
            .send(Connect {
                addr: addr.recipient(),
            })
            .into_actor(self)
            .then(|res, act, ctx| {
                match res {
                    Ok(res) => {
                        act.id = res;
                        act.app.clone().extensions.read().call_connected(act, ctx);
                        debug!("Session started {} {}", act.id, act.ip);
                    }
                    // something is wrong with server
                    _ => {
                        increment_counter!("nostr_relay_session_stop_total", "reason" => "server error");
                        ctx.stop()
                    },
                }
                fut::ready(())
            })
            .wait(ctx);
    }

    fn stopping(&mut self, _: &mut Self::Context) -> Running {
        // notify server
        self.server.do_send(Disconnect { id: self.id });
        Running::Stop
    }

    fn stopped(&mut self, ctx: &mut Self::Context) {
        decrement_gauge!("nostr_relay_session", 1.0);
        self.app
            .clone()
            .extensions
            .read()
            .call_disconnected(self, ctx);
        debug!("Session stopped {} {}", self.id, self.ip);
    }
}

/// Handler for `ws::Message`
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Session {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        // Text will log after processing
        if !matches!(msg, Ok(Message::Text(_)) | Ok(Message::Continuation(_))) {
            debug!("Session message {} {} {:?}", self.id, self.ip, msg);
        }
        let msg = match msg {
            Err(err) => {
                match err {
                    ws::ProtocolError::Overflow => {
                        ctx.text(OutgoingMessage::notice("payload reached size limit."));
                    }
                    _ => {
                        debug!("Session error {} {} {:?}", self.id, self.ip, err);
                        increment_counter!("nostr_relay_session_stop_total", "reason" => "message error");
                        ctx.stop();
                    }
                }
                return;
            }
            Ok(msg) => msg,
        };
        match msg {
            ws::Message::Ping(msg) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            ws::Message::Pong(_) => {
                self.hb = Instant::now();
            }
            ws::Message::Text(text) => {
                let text = text.to_string();
                debug!("Session text {} {} {}", self.id, self.ip, text);
                self.handle_message(text, ctx);
            }
            ws::Message::Close(reason) => {
                ctx.close(reason);
                increment_counter!("nostr_relay_session_stop_total", "reason" => "message close");
                ctx.stop();
            }
            ws::Message::Binary(_) => {
                ctx.text(OutgoingMessage::notice("Not support binary message"));
            }
            ws::Message::Continuation(cont) => match cont {
                Item::FirstText(buf) => {
                    let mut bytes = BytesMut::new();
                    bytes.extend_from_slice(&buf);
                    self.cont = Some(bytes);
                }
                Item::FirstBinary(_) => {
                    ctx.text(OutgoingMessage::notice("Not support binary message"));
                }
                Item::Continue(buf) => {
                    if let Some(bytes) = &mut self.cont {
                        bytes.extend_from_slice(&buf);
                    }
                }
                Item::Last(buf) => {
                    if let Some(mut bytes) = self.cont.take() {
                        bytes.extend_from_slice(&buf);
                        if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                            debug!("Session text {} {} {}", self.id, self.ip, text);
                            self.handle_message(text, ctx);
                        }
                    }
                }
            },
            ws::Message::Nop => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{create_test_app, Extension, ExtensionMessageResult};
    use actix_rt::time::sleep;
    use actix_web_actors::ws;
    use anyhow::Result;
    use bytes::Bytes;
    use futures_util::{SinkExt as _, StreamExt as _};

    #[actix_rt::test]
    async fn pingpong() -> Result<()> {
        let mut srv = actix_test::start(|| {
            let data = create_test_app("session").unwrap();
            data.web_app()
        });

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        framed.send(ws::Message::Ping("text".into())).await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Pong(Bytes::copy_from_slice(b"text")));

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));
        Ok(())
    }

    #[actix_rt::test]
    async fn heartbeat() -> Result<()> {
        let mut srv = actix_test::start(|| {
            let data = create_test_app("session").unwrap();
            {
                let mut w = data.setting.write();
                w.network.heartbeat_interval = Duration::from_secs(1).try_into().unwrap();
                w.network.heartbeat_timeout = Duration::from_secs(20).try_into().unwrap();
            }
            data.web_app()
        });

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        sleep(Duration::from_secs(3)).await;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Ping(Bytes::copy_from_slice(b"")));

        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Ping(Bytes::copy_from_slice(b"")));

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        Ok(())
    }

    #[actix_rt::test]
    async fn heartbeat_timeout() -> Result<()> {
        let mut srv = actix_test::start(|| {
            let data = create_test_app("session").unwrap();
            {
                let mut w = data.setting.write();
                w.network.heartbeat_interval = Duration::from_secs(1).try_into().unwrap();
                w.network.heartbeat_timeout = Duration::from_secs(2).try_into().unwrap();
            }
            data.web_app()
        });
        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        sleep(Duration::from_secs(3)).await;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Ping(Bytes::copy_from_slice(b"")));
        let item = framed.next().await;
        assert!(item.is_none());
        Ok(())
    }

    struct Echo;
    impl Extension for Echo {
        fn message(
            &self,
            msg: ClientMessage,
            _session: &mut Session,
            _ctx: &mut <Session as actix::Actor>::Context,
        ) -> ExtensionMessageResult {
            ExtensionMessageResult::Stop(OutgoingMessage(msg.text))
        }

        fn name(&self) -> &'static str {
            "Echo"
        }
    }

    #[actix_rt::test]
    async fn extension() -> Result<()> {
        let text = r#"["REQ", "1", {}]"#;
        let mut srv = actix_test::start(|| {
            let data = create_test_app("extension").unwrap();
            data.add_extension(Echo).web_app()
        });
        let mut framed = srv.ws_at("/").await.unwrap();
        framed.send(ws::Message::Text(text.into())).await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(
            item,
            ws::Frame::Text(Bytes::copy_from_slice(text.as_bytes()))
        );
        Ok(())
    }

    #[actix_rt::test]
    async fn max_size() -> Result<()> {
        let text = r#"["REQ", "1", {}]"#;
        let max_size = text.len() + 1;
        let mut srv = actix_test::start(move || {
            let data = create_test_app("max_size").unwrap();
            {
                let mut w = data.setting.write();
                w.limitation.max_message_length = max_size;
            }
            data.add_extension(Echo).web_app()
        });
        let mut framed = srv.ws_at("/").await.unwrap();
        framed.send(ws::Message::Text(text.into())).await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(
            item,
            ws::Frame::Text(Bytes::copy_from_slice(text.as_bytes()))
        );

        framed
            .send(ws::Message::Text(format!("{}  ", text).into()))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(
            item,
            ws::Frame::Text(Bytes::copy_from_slice(
                br#"["NOTICE","payload reached size limit."]"#
            ))
        );
        Ok(())
    }

    #[actix_rt::test]
    async fn continuation() -> Result<()> {
        let text = br#"["REQ", "1", {}]"#;

        let mut srv = actix_test::start(|| {
            let data = create_test_app("extension").unwrap();
            data.add_extension(Echo).web_app()
        });
        let mut framed = srv.ws_at("/").await.unwrap();
        framed
            .send(ws::Message::Continuation(Item::FirstText(
                Bytes::copy_from_slice(&text[0..2]),
            )))
            .await?;

        framed
            .send(ws::Message::Continuation(Item::Continue(
                Bytes::copy_from_slice(&text[2..4]),
            )))
            .await?;
        framed
            .send(ws::Message::Continuation(Item::Last(
                Bytes::copy_from_slice(&text[4..]),
            )))
            .await?;

        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Text(Bytes::copy_from_slice(text)));

        Ok(())
    }
}
