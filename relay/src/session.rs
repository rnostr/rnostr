use crate::{message::*, AppData, Server};
use actix::prelude::*;
use actix_web_actors::ws;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Session {
    pub ip: String,

    /// unique session id
    pub id: usize,

    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    pub hb: Instant,

    /// server
    pub server: Addr<Server>,

    /// heartbeat timeout
    /// How long before lack of client response causes a timeout
    pub heartbeat_timeout: Duration,

    /// heartbeat interval
    /// How often heartbeat pings are sent
    pub heartbeat_interval: Duration,
}

impl Session {
    pub fn new(ip: String, data: &AppData) -> Session {
        let setting = data.setting.read();
        Self {
            id: 0,
            ip,
            hb: Instant::now(),
            server: data.server.clone(),
            heartbeat_timeout: Duration::from_secs(setting.session.heartbeat_timeout),
            heartbeat_interval: Duration::from_secs(setting.session.heartbeat_interval),
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
                    Ok(res) => act.id = res,
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

    fn stopped(&mut self, _ctx: &mut Self::Context) {}
}

/// Handler for `ws::Message`
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Session {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        // debug!("WEBSOCKET MESSAGE: {msg:?}");
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
                    Ok(msg) => {
                        self.server.do_send(ClientMessage {
                            id: self.id,
                            text,
                            msg,
                        });
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
    use crate::create_app;
    use crate::{temp_db_path, PROMETHEUS_HANDLE};
    use actix_rt::time::sleep;
    use actix_web_actors::ws;
    use anyhow::Result;
    use bytes::Bytes;
    use futures_util::{SinkExt as _, StreamExt as _};

    #[actix_rt::test]
    async fn pingpong() -> Result<()> {
        let data = AppData::create(Some(temp_db_path("session")?), PROMETHEUS_HANDLE.clone())?;

        let mut srv = actix_test::start(move || create_app(data.clone()));

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
        let data = AppData::create(Some(temp_db_path("session")?), PROMETHEUS_HANDLE.clone())?;
        {
            let mut w = data.setting.write();
            w.session.heartbeat_interval = 1;
            w.session.heartbeat_timeout = 20;
        }
        let mut srv = actix_test::start(move || create_app(data.clone()));

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
        let data = AppData::create(Some(temp_db_path("session")?), PROMETHEUS_HANDLE.clone())?;
        {
            let mut w = data.setting.write();
            w.session.heartbeat_interval = 1;
            w.session.heartbeat_timeout = 2;
        }
        let mut srv = actix_test::start(move || create_app(data.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        sleep(Duration::from_secs(3)).await;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Ping(Bytes::copy_from_slice(b"")));
        let item = framed.next().await;
        assert!(item.is_none());
        Ok(())
    }
}
