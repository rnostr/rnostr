use actix::{Handler, Message, StreamHandler};
use actix_codec::Decoder;
use actix_http::ws::{CloseCode, Codec};
use actix_web::{error::PayloadError, web};
use actix_web_actors::ws;
use bytes::BytesMut;
use futures_util::{stream, Stream, StreamExt};
use nostr_extensions::auth::{Auth, AuthState};
use nostr_relay::{
    db::{
        now,
        secp256k1::{rand::thread_rng, Keypair, XOnlyPublicKey},
        Event,
    },
    message::{ClientMessage, IncomingMessage, OutgoingMessage},
    App, Session,
};
use serde_json::{json, Value};
use std::{fs, time::Duration};

#[derive(Message)]
#[rtype(result = "()")]
struct Request(Option<String>, String);

impl Handler<Request> for Session {
    type Result = ();

    fn handle(&mut self, request: Request, ctx: &mut Self::Context) {
        if let Some(identity) = request.0 {
            self.set(AuthState::Pubkey(identity));
        }
        <Self as StreamHandler<_>>::handle(self, Ok(ws::Message::Text(request.1.into())), ctx);
    }
}

fn decode_frames(bytes: &[u8]) -> anyhow::Result<Vec<ws::Frame>> {
    let mut codec = Codec::new().client_mode();
    let mut bytes = BytesMut::from(bytes);
    let mut frames = Vec::new();
    while let Some(frame) = codec.decode(&mut bytes)? {
        frames.push(frame);
    }
    anyhow::ensure!(bytes.is_empty(), "incomplete WebSocket frame");
    Ok(frames)
}

fn text_messages(bytes: &[u8]) -> anyhow::Result<Vec<Value>> {
    decode_frames(bytes)?
        .into_iter()
        .map(|frame| match frame {
            ws::Frame::Text(text) => Ok(serde_json::from_slice(&text)?),
            other => anyhow::bail!("expected text frame, got {other:?}"),
        })
        .collect()
}

async fn publish(app: &App, key: &Keypair, content: &str) -> anyhow::Result<()> {
    let event = Event::create(key, now(), 1, vec![], content.into())?;
    app.server
        .send(ClientMessage::new(
            0,
            String::new(),
            IncomingMessage::Event(event),
        ))
        .await?;
    Ok(())
}

async fn read_until(
    output: &mut (impl Stream<Item = Result<web::Bytes, actix_web::Error>> + Unpin),
    expected: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    actix_rt::time::timeout(Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        while let Some(chunk) = output.next().await {
            bytes.extend(chunk.map_err(|error| anyhow::anyhow!("{error}"))?);
            if expected.is_some_and(|text| {
                bytes
                    .windows(text.len())
                    .any(|part| part == text.as_bytes())
            }) {
                return Ok(bytes);
            }
        }
        anyhow::ensure!(expected.is_none(), "session closed before {expected:?}");
        Ok(bytes)
    })
    .await?
}

#[actix_rt::test]
async fn reloaded_read_permissions_revoke_open_subscriptions() -> anyhow::Result<()> {
    for (initial, revoked, sub_id) in [
        (
            json!({"ip_blacklist": []}),
            json!({"ip_blacklist": ["127.0.0.1"]}),
            "x\"",
        ),
        (
            json!({"pubkey_whitelist": ["reader", "keeper"]}),
            json!({"pubkey_whitelist": ["keeper"]}),
            "x\\",
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let config = dir.path().join("relay.json");
        fs::write(
            &config,
            json!({"auth": {"enabled": true, "req": initial}, "thread": {"reader": 1}}).to_string(),
        )?;
        let app = web::Data::new(
            App::create(Some(config.as_path()), false, None, Some(dir.path()))?
                .add_extension(Auth::new())?,
        );
        let (addr, output) = ws::WebsocketContext::create_with_addr(
            Session::new("127.0.0.1".into(), app.clone()),
            stream::pending::<Result<web::Bytes, PayloadError>>(),
        );
        let mut output = Box::pin(output);
        addr.do_send(Request(
            Some("reader".into()),
            json!(["REQ", sub_id, {}]).to_string(),
        ));
        let messages = text_messages(&read_until(&mut output, Some("\"EOSE\"")).await?)?;
        assert!(messages.contains(&json!(["EOSE", sub_id])), "{messages:?}");
        let key = Keypair::new_global(&mut thread_rng());
        publish(&app, &key, "before").await?;
        let messages = text_messages(&read_until(&mut output, Some("before")).await?)?;
        assert!(
            messages
                .iter()
                .any(|msg| msg[0] == "EVENT" && msg[1] == sub_id),
            "{messages:?}"
        );
        fs::write(
            &config,
            json!({"auth": {"enabled": true, "req": revoked}}).to_string(),
        )?;
        app.reload(&config)?;
        addr.do_send(OutgoingMessage::notice("still allowed"));
        read_until(&mut output, Some("still allowed")).await?;
        publish(&app, &key, "after").await?;
        let frames = decode_frames(&read_until(&mut output, None).await?)?;
        assert!(
            matches!(frames.as_slice(), [ws::Frame::Close(Some(reason))] if reason.code == CloseCode::Policy),
            "{frames:?}"
        );
        drop(output);
        assert!(!addr.connected());
    }
    Ok(())
}

#[actix_rt::test]
async fn newly_required_authentication_closes_subscription_and_allows_recovery(
) -> anyhow::Result<()> {
    for enabled in [false, true] {
        let dir = tempfile::tempdir()?;
        let config = dir.path().join("relay.json");
        fs::write(
            &config,
            json!({"auth": {"enabled": enabled}, "thread": {"reader": 1}}).to_string(),
        )?;
        let app = web::Data::new(
            App::create(Some(config.as_path()), false, None, Some(dir.path()))?
                .add_extension(Auth::new())?,
        );
        let (addr, output) = ws::WebsocketContext::create_with_addr(
            Session::new("127.0.0.1".into(), app.clone()),
            stream::pending::<Result<web::Bytes, PayloadError>>(),
        );
        let mut output = Box::pin(output);
        addr.do_send(Request(None, r#"["REQ","sub",{}]"#.into()));
        let mut messages = text_messages(&read_until(&mut output, Some(r#""EOSE","sub""#)).await?)?;
        let key = Keypair::new_global(&mut thread_rng());
        let pubkey = XOnlyPublicKey::from_keypair(&key).0.to_string();
        fs::write(
            &config,
            json!({"auth": {"enabled": true, "req": {"pubkey_whitelist": [pubkey]}}}).to_string(),
        )?;
        app.reload(&config)?;
        publish(&app, &key, "blocked").await?;
        messages.extend(text_messages(
            &read_until(&mut output, Some(r#""CLOSED","sub""#)).await?,
        )?);
        addr.do_send(OutgoingMessage(
            r#" [ "\u0045VENT", "sub", {"content":"queued"} ] "#.into(),
        ));
        addr.do_send(OutgoingMessage::notice("ready"));
        messages.extend(text_messages(
            &read_until(&mut output, Some("ready")).await?,
        )?);
        let closed: Vec<_> = messages.iter().filter(|msg| msg[0] == "CLOSED").collect();
        assert_eq!(closed.len(), 1, "{messages:?}");
        assert_eq!(
            closed[0],
            &json!(["CLOSED", "sub", "auth-required: NIP-42 auth required"])
        );
        assert!(
            !messages.iter().any(|msg| msg[0] == "EVENT"),
            "{messages:?}"
        );
        let challenge = messages
            .iter()
            .find(|msg| msg[0] == "AUTH")
            .and_then(|msg| msg[1].as_str())
            .expect("AUTH challenge");
        let auth = Event::create(
            &key,
            now(),
            22242,
            vec![vec!["challenge".into(), challenge.into()]],
            String::new(),
        )?;
        addr.do_send(Request(None, format!(r#"["AUTH",{auth}]"#)));
        let reply = text_messages(&read_until(&mut output, Some(&auth.id_str())).await?)?;
        assert!(
            reply
                .iter()
                .any(|msg| msg == &json!(["OK", auth.id_str(), true, ""])),
            "{reply:?}"
        );
        addr.do_send(OutgoingMessage::event("sub", r#"{"content":"stale"}"#));
        addr.do_send(OutgoingMessage::eose("sub"));
        addr.do_send(OutgoingMessage::notice("authenticated"));
        let reply = text_messages(&read_until(&mut output, Some("authenticated")).await?)?;
        assert_eq!(reply, vec![json!(["NOTICE", "authenticated"])]);
        addr.do_send(Request(None, r#"["REQ","sub",{}]"#.into()));
        let reply = text_messages(&read_until(&mut output, Some(r#""EOSE","sub""#)).await?)?;
        assert!(reply.iter().any(|msg| msg[0] == "EVENT"), "{reply:?}");
        assert!(addr.connected());
    }
    Ok(())
}
