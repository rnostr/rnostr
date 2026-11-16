use crate::{hash::NoOpHasherDefault, message::*, App, Server};
use actix::prelude::*;
use actix_http::ws::Item;
use actix_web::web;
use actix_web_actors::ws;
use bytes::BytesMut;
use metrics::{counter, gauge};
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
                counter!("nostr_relay_session_stop_total", "reason" => "heartbeat timeout")
                    .increment(1);
                ctx.stop();
                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
    }

    fn send_error(
        &self,
        err: crate::Error,
        msg: &ClientMessage,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        if let IncomingMessage::Event(event) = &msg.msg {
            ctx.text(OutgoingMessage::ok(
                &event.id_str(),
                false,
                &err.to_string(),
            ));
        } else if let IncomingMessage::Req(sub) = &msg.msg {
            ctx.text(OutgoingMessage::closed(&sub.id, &err.to_string()));
        } else {
            ctx.text(OutgoingMessage::notice(&err.to_string()));
        }
    }

    fn handle_message(&mut self, text: String, ctx: &mut ws::WebsocketContext<Self>) {
        let msg = serde_json::from_str::<IncomingMessage>(&text);
        match msg {
            Ok(msg) => {
                if let Some(cmd) = msg.known_command() {
                    // only insert known command metrics
                    counter!("nostr_relay_message_total", "command" => cmd).increment(1);
                }

                let mut msg = ClientMessage::new(self.id, text, msg);
                {
                    let r = self.app.setting.read();
                    if let Err(err) = msg.validate(&r.limitation) {
                        self.send_error(err, &msg, ctx);
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
                        if let Err(err) = msg.validate_nip70() {
                            self.send_error(err, &msg, ctx);
                            return;
                        }
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
        counter!("nostr_relay_session_total").increment(1);
        gauge!("nostr_relay_session").increment(1.0);

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
                        counter!("nostr_relay_session_stop_total", "reason" => "server error")
                            .increment(1);
                        ctx.stop()
                    }
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
        gauge!("nostr_relay_session").decrement(1.0);
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
                        self.cont = None;
                        ctx.text(OutgoingMessage::notice("payload reached size limit."));
                        ctx.close(Some(ws::CloseCode::Size.into()));
                    }
                    _ => {
                        debug!("Session error {} {} {:?}", self.id, self.ip, err);
                    }
                }
                counter!("nostr_relay_session_stop_total", "reason" => "message error")
                    .increment(1);
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
                debug!("Session text {} {} {}", self.id, self.ip, text);
                self.handle_message(text, ctx);
            }
            ws::Message::Close(reason) => {
                ctx.close(reason);
                counter!("nostr_relay_session_stop_total", "reason" => "message close")
                    .increment(1);
                ctx.stop();
            }
            ws::Message::Binary(_) => {
                ctx.text(OutgoingMessage::notice("Not support binary message"));
            }
            ws::Message::Continuation(cont) => {
                let (buf, last) = match cont {
                    Item::FirstText(buf) => {
                        self.cont = Some(BytesMut::new());
                        (buf, false)
                    }
                    Item::FirstBinary(_) => {
                        self.cont = None;
                        ctx.text(OutgoingMessage::notice("Not support binary message"));
                        return;
                    }
                    Item::Continue(buf) => (buf, false),
                    Item::Last(buf) => (buf, true),
                };
                let Some(mut bytes) = self.cont.take() else {
                    return;
                };
                let limit = self.app.setting.read().limitation.max_message_length;
                if !bytes
                    .len()
                    .checked_add(buf.len())
                    .is_some_and(|len| len <= limit)
                {
                    ctx.text(OutgoingMessage::notice("payload reached size limit."));
                    return;
                }
                bytes.extend_from_slice(&buf);
                if last {
                    match String::from_utf8(Vec::from(bytes)) {
                        Ok(text) => {
                            debug!("Session text {} {} {}", self.id, self.ip, text);
                            self.handle_message(text, ctx);
                        }
                        Err(_) => {
                            ctx.text(OutgoingMessage::notice(
                                "invalid UTF-8 in fragmented text message.",
                            ));
                            ctx.close(Some(ws::CloseCode::Invalid.into()));
                            counter!("nostr_relay_session_stop_total", "reason" => "message error")
                                .increment(1);
                            ctx.stop();
                        }
                    }
                } else {
                    self.cont = Some(bytes);
                }
            }
            ws::Message::Nop => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{create_test_app, Extension, ExtensionMessageResult};
    use actix_http::ws::{OpCode, Parser};
    use actix_rt::time::{sleep, timeout};
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
            data.add_extension(Echo).unwrap().web_app()
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
            data.add_extension(Echo).unwrap().web_app()
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
        let close = timeout(Duration::from_secs(5), framed.next())
            .await?
            .unwrap()?;
        assert_eq!(close, ws::Frame::Close(Some(ws::CloseCode::Size.into())));
        Ok(())
    }

    async fn assert_websocket_rejected(
        input: Bytes,
        notice: &str,
        close: ws::CloseCode,
    ) -> Result<()> {
        let app = create_test_app("websocket_rejection")?.add_extension(Echo)?;
        app.setting.write().limitation.max_message_length = 32;
        let session = Session::new("127.0.0.1".into(), web::Data::new(app));
        let input = futures_util::stream::iter([Ok::<_, actix_web::error::PayloadError>(input)])
            .chain(futures_util::stream::pending());
        let output = ws::WebsocketContext::with_codec(
            session,
            input,
            actix_http::ws::Codec::new().max_size(32),
        );
        futures_util::pin_mut!(output);
        let chunk = timeout(Duration::from_secs(5), output.next())
            .await?
            .unwrap()
            .expect("the WebSocket context must emit a valid response");
        let mut bytes = BytesMut::from(chunk.as_ref());
        let mut replies = Vec::new();
        while let Some((finished, opcode, payload)) = Parser::parse(&mut bytes, false, 1024)? {
            assert!(finished);
            replies.push((opcode, payload.unwrap_or_default().freeze()));
        }
        assert!(bytes.is_empty());
        assert_eq!(
            replies,
            [
                (OpCode::Text, Bytes::copy_from_slice(notice.as_bytes())),
                (
                    OpCode::Close,
                    Bytes::copy_from_slice(&u16::from(close).to_be_bytes()),
                ),
            ]
        );
        assert!(timeout(Duration::from_secs(5), output.next())
            .await?
            .is_none());
        Ok(())
    }

    #[actix_rt::test]
    async fn decoder_overflow_closes() -> Result<()> {
        let mut input = BytesMut::from(&[0x81, 0xff][..]);
        input.extend_from_slice(&(1u64 << 63).to_be_bytes());
        input.extend_from_slice(&[0; 4]);
        assert_websocket_rejected(
            input.freeze(),
            r#"["NOTICE","payload reached size limit."]"#,
            ws::CloseCode::Size,
        )
        .await
    }

    #[actix_rt::test]
    async fn continuation_frame_overflow_closes() -> Result<()> {
        let text = br#"["REQ","1",{}]"#;
        for position in 0..3 {
            let mut input = BytesMut::new();
            if position > 0 {
                Parser::write_message(&mut input, text, OpCode::Text, false, true);
            }
            Parser::write_message(
                &mut input,
                [b' '; 33],
                if position == 0 {
                    OpCode::Text
                } else {
                    OpCode::Continue
                },
                position == 2,
                true,
            );
            if position < 2 {
                Parser::write_message(&mut input, [], OpCode::Continue, true, true);
            }
            Parser::write_message(&mut input, text, OpCode::Text, true, true);
            assert_websocket_rejected(
                input.freeze(),
                r#"["NOTICE","payload reached size limit."]"#,
                ws::CloseCode::Size,
            )
            .await?;
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn continuation_invalid_utf8_closes() -> Result<()> {
        let mut input = BytesMut::new();
        Parser::write_message(&mut input, b"[", OpCode::Text, false, true);
        Parser::write_message(&mut input, b"\xff", OpCode::Continue, true, true);
        Parser::write_message(&mut input, br#"["REQ","1",{}]"#, OpCode::Text, true, true);
        assert_websocket_rejected(
            input.freeze(),
            r#"["NOTICE","invalid UTF-8 in fragmented text message."]"#,
            ws::CloseCode::Invalid,
        )
        .await
    }

    async fn continuation_replies(
        scenario: impl FnOnce(&mut Session, &mut ws::WebsocketContext<Session>) + 'static,
    ) -> Result<Vec<Bytes>> {
        let app = create_test_app("continuation_limits")?;
        app.setting.write().limitation.max_message_length = 32;
        let app = web::Data::new(app.add_extension(Echo)?);
        let output = ws::WebsocketContext::with_factory(
            futures_util::stream::pending::<Result<Bytes, actix_web::error::PayloadError>>(),
            move |ctx| {
                let mut session = Session::new("127.0.0.1".into(), app);
                scenario(&mut session, ctx);
                session
            },
        );
        futures_util::pin_mut!(output);
        let chunk = timeout(Duration::from_secs(5), output.next())
            .await?
            .unwrap()
            .expect("the WebSocket context must emit a valid response");
        let mut bytes = BytesMut::from(chunk.as_ref());
        let mut replies = Vec::new();
        while let Some((finished, opcode, payload)) =
            actix_http::ws::Parser::parse(&mut bytes, false, 1024)?
        {
            assert!(finished);
            assert_eq!(opcode, actix_http::ws::OpCode::Text);
            replies.push(payload.unwrap().freeze());
        }
        assert!(bytes.is_empty());
        Ok(replies)
    }

    #[actix_rt::test]
    async fn continuation_buffer_size_limit() -> Result<()> {
        let text = br#"["REQ","1",{}]"#;
        let replies = continuation_replies(move |session, ctx| {
            for fragment in [
                Item::FirstText(Bytes::from_static(text)),
                Item::Continue(Bytes::from_static(b"        ")),
                Item::Continue(Bytes::from_static(b"        ")),
                Item::Continue(Bytes::from_static(b"        ")),
            ] {
                StreamHandler::handle(session, Ok(Message::Continuation(fragment)), ctx);
            }
            assert!(
                session.cont.is_none(),
                "an oversized message must release its buffer"
            );
            for fragment in [
                Item::Continue(Bytes::from_static(text)),
                Item::Last(Bytes::from_static(b"        ")),
                Item::FirstText(Bytes::from_static(&text[..2])),
                Item::Last(Bytes::from_static(&text[2..])),
            ] {
                StreamHandler::handle(session, Ok(Message::Continuation(fragment)), ctx);
            }
        })
        .await?;
        assert_eq!(
            replies,
            [
                Bytes::from_static(br#"["NOTICE","payload reached size limit."]"#),
                Bytes::from_static(text),
            ]
        );
        Ok(())
    }

    #[actix_rt::test]
    async fn continuation_boundaries_and_utf8() -> Result<()> {
        let text = br#"["REQ","1",{}]"#;
        let unicode = r#"["REQ","é",{}]"#.as_bytes();
        let split = unicode.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
        let replies = continuation_replies(move |session, ctx| {
            for fragment in [
                Item::FirstText(Bytes::from_static(text)),
                Item::Continue(Bytes::from_static(b"        ")),
                Item::Last(Bytes::from_static(b"          ")),
                Item::FirstText(Bytes::from_static(text)),
                Item::Last(Bytes::from_static(b"                   ")),
                Item::FirstText(Bytes::copy_from_slice(&unicode[..split])),
                Item::Last(Bytes::copy_from_slice(&unicode[split..])),
            ] {
                StreamHandler::handle(session, Ok(Message::Continuation(fragment)), ctx);
            }
            assert!(session.cont.is_none());
            StreamHandler::handle(
                session,
                Ok(Message::Text(std::str::from_utf8(text).unwrap().into())),
                ctx,
            );
        })
        .await?;
        assert_eq!(
            replies,
            [
                Bytes::from(format!("{}{}", std::str::from_utf8(text)?, " ".repeat(18))),
                Bytes::from_static(br#"["NOTICE","payload reached size limit."]"#),
                Bytes::from_static(unicode),
                Bytes::from_static(text),
            ]
        );
        Ok(())
    }

    #[actix_rt::test]
    async fn continuation_uses_current_size_limit() -> Result<()> {
        let text = br#"["REQ","1",{}]"#;
        let replies = continuation_replies(move |session, ctx| {
            for (limit, fragment) in [
                (32, Item::FirstText(Bytes::from_static(text))),
                (8, Item::Continue(Bytes::new())),
                (8, Item::Last(Bytes::new())),
                (32, Item::FirstText(Bytes::from_static(text))),
                (8, Item::Last(Bytes::new())),
                (8, Item::FirstText(Bytes::from_static(text))),
                (8, Item::Last(Bytes::new())),
                (64, Item::FirstText(Bytes::from_static(text))),
                (64, Item::Last(Bytes::from_static(b"                   "))),
            ] {
                session.app.setting.write().limitation.max_message_length = limit;
                StreamHandler::handle(session, Ok(Message::Continuation(fragment)), ctx);
                if let Some(bytes) = &session.cont {
                    assert!(bytes.len() <= limit);
                }
            }
        })
        .await?;
        assert_eq!(
            replies,
            [
                Bytes::from_static(br#"["NOTICE","payload reached size limit."]"#),
                Bytes::from_static(br#"["NOTICE","payload reached size limit."]"#),
                Bytes::from_static(br#"["NOTICE","payload reached size limit."]"#),
                Bytes::from(format!("{}{}", std::str::from_utf8(text)?, " ".repeat(19))),
            ]
        );
        Ok(())
    }

    #[actix_rt::test]
    async fn continuation_size_limit() -> Result<()> {
        let text = br#"["REQ","1",{}]"#;
        let mut srv = actix_test::start(|| {
            let data = create_test_app("continuation_size_limit").unwrap();
            data.setting.write().limitation.max_message_length = 32;
            data.add_extension(Echo).unwrap().web_app()
        });
        let mut framed = srv.ws_at("/").await.unwrap();
        framed
            .send(ws::Message::Continuation(Item::FirstText(
                Bytes::from_static(text),
            )))
            .await?;
        for _ in 0..3 {
            framed
                .send(ws::Message::Continuation(Item::Continue(
                    Bytes::from_static(b"        "),
                )))
                .await?;
        }
        let notice = timeout(Duration::from_secs(5), framed.next())
            .await
            .expect("the total size limit must be enforced before the final fragment")
            .unwrap()?;
        assert_eq!(
            notice,
            ws::Frame::Text(Bytes::from_static(
                br#"["NOTICE","payload reached size limit."]"#
            ))
        );
        for fragment in [
            Item::Continue(Bytes::from_static(text)),
            Item::Continue(Bytes::from_static(b"        ")),
            Item::Last(Bytes::from_static(b"        ")),
        ] {
            framed.send(ws::Message::Continuation(fragment)).await?;
        }
        for fragment in [
            Item::FirstText(Bytes::from_static(&text[..2])),
            Item::Last(Bytes::from_static(&text[2..])),
        ] {
            framed.send(ws::Message::Continuation(fragment)).await?;
        }
        let reply = timeout(Duration::from_secs(5), framed.next())
            .await?
            .unwrap()?;
        assert_eq!(reply, ws::Frame::Text(Bytes::from_static(text)));
        Ok(())
    }

    #[actix_rt::test]
    async fn continuation() -> Result<()> {
        let text = br#"["REQ", "1", {}]"#;

        let mut srv = actix_test::start(|| {
            let data = create_test_app("extension").unwrap();
            data.add_extension(Echo).unwrap().web_app()
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
