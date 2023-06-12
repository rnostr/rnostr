use crate::{message::*, App, Server};
use actix::prelude::*;
use actix_web::web;
use actix_web_actors::ws;
use metrics::{decrement_gauge, increment_counter, increment_gauge};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tracing::debug;

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

    app: web::Data<App>,

    /// Simple store for save extension data
    pub data: HashMap<String, String>,
}

impl Session {
    /// Get session id
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn new(ip: String, app: web::Data<App>) -> Session {
        let setting = app.setting.read();
        let heartbeat_timeout = Duration::from_secs(setting.network.heartbeat_timeout);
        let heartbeat_interval = Duration::from_secs(setting.network.heartbeat_interval);
        drop(setting);
        Self {
            id: 0,
            ip,
            hb: Instant::now(),
            server: app.server.clone(),
            heartbeat_timeout,
            heartbeat_interval,
            app,
            data: HashMap::new(),
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
                ctx.stop();
                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
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
        increment_counter!("new_connections");
        increment_gauge!("current_connections", 1.0);

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
                        act.app.clone().extensions.call_connected(act, ctx);
                        debug!("Session started {:?} {:?}", act.id, act.ip);
                    }
                    // something is wrong with server
                    _ => ctx.stop(),
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
        decrement_gauge!("current_connections", 1.0);
        self.app.clone().extensions.call_disconnected(self, ctx);
        debug!("Session stopped {:?} {:?}", self.id, self.ip);
    }
}

/// Handler for `ws::Message`
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Session {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        debug!("Session message {:?} {:?} {:?}", self.id, self.ip, msg);
        let msg = match msg {
            Err(_err) => {
                ctx.stop();
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
                let msg = serde_json::from_str::<IncomingMessage>(&text);
                match msg {
                    // TODO: validate, fill limit
                    Ok(msg) => {
                        let msg = ClientMessage {
                            id: self.id,
                            text,
                            msg,
                        };
                        match self.app.clone().extensions.call_message(msg, self, ctx) {
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
                        ctx.text(OutgoingMessage::notice(&format!(
                            "json error: {}",
                            err.to_string()
                        )));
                    }
                };
            }
            ws::Message::Close(reason) => {
                ctx.close(reason);
                ctx.stop();
            }
            ws::Message::Binary(_) => {
                ctx.text(OutgoingMessage::notice("Not support binary message"));
            }
            ws::Message::Continuation(_) => {
                ctx.stop();
            }
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
                w.network.heartbeat_interval = 1;
                w.network.heartbeat_timeout = 20;
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
                w.network.heartbeat_interval = 1;
                w.network.heartbeat_timeout = 2;
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

    struct Ext;
    impl Extension for Ext {
        fn message(
            &self,
            _msg: ClientMessage,
            _session: &mut Session,
            _ctx: &mut <Session as actix::Actor>::Context,
        ) -> ExtensionMessageResult {
            ExtensionMessageResult::Stop(OutgoingMessage::notice("extension"))
        }
    }

    #[actix_rt::test]
    async fn extension() -> Result<()> {
        let mut srv = actix_test::start(|| {
            let data = create_test_app("session").unwrap();
            data.add_extension(Ext).web_app()
        });
        let mut framed = srv.ws_at("/").await.unwrap();
        framed
            .send(ws::Message::Text(r#"["REQ", "1", {}]"#.into()))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(
            item,
            ws::Frame::Text(Bytes::copy_from_slice(br#"["NOTICE","extension"]"#))
        );
        Ok(())
    }
}
