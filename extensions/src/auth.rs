use metrics::{counter, describe_counter};
use nostr_relay::db::now;
use nostr_relay::{
    message::{ClientMessage, IncomingMessage, OutgoingMessage},
    Extension, ExtensionMessageResult, List, Result, Session, Setting, SettingUpdate,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize, Default, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Permission {
    pub ip_whitelist: Option<List>,
    pub pubkey_whitelist: Option<List>,
    pub ip_blacklist: Option<List>,
    pub pubkey_blacklist: Option<List>,
    pub event_pubkey_whitelist: Option<List>,
    pub event_pubkey_blacklist: Option<List>,
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub allow_mentioning_whitelisted_pubkeys: bool,
}

impl Permission {
    fn restricts_writes(&self) -> bool {
        self.ip_whitelist.is_some()
            || self.pubkey_whitelist.is_some()
            || self.event_pubkey_whitelist.is_some()
            // Unlike IP or event-author blacklists, this requires login before every write,
            // even when empty, so we advertise the publishing prerequisite.
            || self.pubkey_blacklist.is_some()
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct AuthSetting {
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub enabled: bool,
    /// read auth: ["REQ"]
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_table")]
    pub req: Option<Permission>,
    /// write auth: ["EVENT"]
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_table")]
    pub event: Option<Permission>,
}

#[derive(Default, Debug)]
pub struct Auth {
    setting: AuthSetting,
}

pub enum AuthState {
    /// The AUTH challenge
    Challenge(String),
    /// Authenticated with pubkey
    Pubkey(String),
}

impl AuthState {
    pub fn authed(&self) -> bool {
        matches!(self, Self::Pubkey(_))
    }

    pub fn pubkey(&self) -> Option<&String> {
        match self {
            Self::Pubkey(p) => Some(p),
            Self::Challenge(_) => None,
        }
    }
}

impl Auth {
    pub fn new() -> Self {
        describe_counter!(
            "nostr_relay_auth_unauthorized",
            "The total count of unauthorized messages"
        );
        Self {
            setting: AuthSetting::default(),
        }
    }

    pub fn verify_permission(
        permission: Option<&Permission>,
        pubkey: Option<&String>,
        event_pubkey: Option<&String>,
        event_tags: Option<&Vec<Vec<String>>>,
        ip: &String,
    ) -> Result<(), &'static str> {
        if let Some(permission) = permission {
            if let Some(list) = &permission.ip_whitelist {
                if !list.contains(ip) {
                    return Err("ip not in whitelist");
                }
            }
            if let Some(list) = &permission.ip_blacklist {
                if list.contains(ip) {
                    return Err("ip in blacklist");
                }
            }

            if let Some(pubkey) = event_pubkey {
                if let Some(list) = &permission.event_pubkey_whitelist {
                    let whitelisted_pubkey_is_mentioned =
                        if permission.allow_mentioning_whitelisted_pubkeys {
                            let mut event_mentioned_pubkeys = event_tags
                                .iter()
                                .flat_map(|i| i.iter())
                                .filter(|t| t.len() > 1 && t[0] == "p")
                                .map(|t| &t[1]);
                            event_mentioned_pubkeys.any(|i| list.contains(i))
                        } else {
                            false
                        };
                    if !whitelisted_pubkey_is_mentioned && !list.contains(pubkey) {
                        return Err("event author pubkey not in whitelist");
                    }
                }
                if let Some(list) = &permission.event_pubkey_blacklist {
                    if list.contains(pubkey) {
                        return Err("event author pubkey in blacklist");
                    }
                }
            }

            if let Some(list) = &permission.pubkey_whitelist {
                if let Some(pubkey) = pubkey {
                    if !list.contains(pubkey) {
                        return Err("pubkey not in whitelist");
                    }
                } else {
                    return Err("NIP-42 auth required");
                }
            }
            if let Some(list) = &permission.pubkey_blacklist {
                if let Some(pubkey) = pubkey {
                    if list.contains(pubkey) {
                        return Err("pubkey in blacklist");
                    }
                } else {
                    return Err("NIP-42 auth required");
                }
            }
        }
        Ok(())
    }
}

impl Extension for Auth {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn setting<'a>(&'a mut self, setting: &mut Setting) -> Result<SettingUpdate<'a>> {
        let parsed: AuthSetting = setting.parse_extension(self.name())?;
        if parsed.enabled {
            setting.add_nip(42);
        }
        let restricted_writes = parsed.enabled
            && parsed
                .event
                .as_ref()
                .is_some_and(Permission::restricts_writes);
        setting.set_restricted_writes(self.name(), restricted_writes);
        Ok(Box::new(move || self.setting = parsed))
    }

    fn connected(&self, session: &mut Session, ctx: &mut <Session as actix::Actor>::Context) {
        if self.setting.enabled {
            let uuid = Uuid::new_v4().to_string();
            let state = AuthState::Challenge(uuid.clone());
            session.set(state);
            ctx.text(format!(r#"["AUTH", "{uuid}"]"#));
        }
    }

    fn message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        _ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        let mut msg = msg;

        if self.setting.enabled {
            let state = session.get::<AuthState>();
            msg.nip70_checked = true;
            match &msg.msg {
                IncomingMessage::Auth(event) => {
                    if let Some(AuthState::Challenge(challenge)) = state {
                        if let Err(err) = event.validate(now(), 0, 0) {
                            return OutgoingMessage::ok(
                                &event.id_str(),
                                false,
                                &format!("auth-required: {}", err),
                            )
                            .into();
                        } else if event.kind() == 22242 {
                            for tag in event.tags() {
                                if tag.len() > 1 && tag[0] == "challenge" && &tag[1] == challenge {
                                    session.set(AuthState::Pubkey(event.pubkey_str()));
                                    return OutgoingMessage::ok(&event.id_str(), true, "").into();
                                }
                            }
                        }
                    }
                    return OutgoingMessage::ok(
                        &event.id_str(),
                        false,
                        "auth-required: need reconnect",
                    )
                    .into();
                }
                IncomingMessage::Event(event) => {
                    if let Err(err) = Self::verify_permission(
                        self.setting.event.as_ref(),
                        state.and_then(|s| s.pubkey()),
                        Some(&event.pubkey_str()),
                        Some(event.tags()),
                        session.ip(),
                    ) {
                        counter!("nostr_relay_auth_unauthorized", "command" => "EVENT", "reason" => err).increment(1);
                        return OutgoingMessage::ok(
                            &event.id_str(),
                            false,
                            &format!("auth-required: {}", err),
                        )
                        .into();
                    } else {
                        // check nip70 protected event
                        for tag in event.tags() {
                            if tag.len() == 1 && tag[0] == "-" {
                                if let Some(AuthState::Pubkey(pubkey)) = state {
                                    if pubkey != &event.pubkey_str() {
                                        return OutgoingMessage::ok(
                                            &event.id_str(),
                                            false,
                                            "auth-required: this event may only be published by its author",
                                        )
                                        .into();
                                    }
                                } else {
                                    return OutgoingMessage::ok(
                                        &event.id_str(),
                                        false,
                                        "auth-required: this event require authorization",
                                    )
                                    .into();
                                }
                                break;
                            }
                        }
                    }
                }
                IncomingMessage::Req(sub) | IncomingMessage::Count(sub) => {
                    if let Err(err) = Self::verify_permission(
                        self.setting.req.as_ref(),
                        state.and_then(|s| s.pubkey()),
                        None,
                        None,
                        session.ip(),
                    ) {
                        counter!("nostr_relay_auth_unauthorized", "command" => "REQ", "reason" => err).increment(1);
                        let msg = format!("auth-required: {}", err);
                        return OutgoingMessage::closed(&sub.id, &msg).into();
                    }
                }
                _ => {}
            }
        }
        ExtensionMessageResult::Continue(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_test_app;
    use actix_web::{
        test::{call_and_read_body_json, init_service, TestRequest},
        web,
    };
    use actix_web_actors::ws;
    use anyhow::Result;
    use futures_util::{SinkExt as _, StreamExt as _};
    use nostr_relay::db::{
        secp256k1::{rand::thread_rng, Keypair, XOnlyPublicKey},
        Event,
    };
    use nostr_relay::{create_web_app, setting::SettingWrapper};
    use serde_json::{json, Value};

    fn parse_text<T: serde::de::DeserializeOwned>(frame: &ws::Frame) -> Result<T> {
        if let ws::Frame::Text(text) = &frame {
            let data: T = serde_json::from_slice(text)?;
            Ok(data)
        } else {
            Err(nostr_relay::Error::Message("invalid frame type".to_string()).into())
        }
    }

    #[actix_rt::test]
    async fn nip11_reports_write_restrictions() -> Result<()> {
        let app = web::Data::new(create_test_app("auth_nip11")?.add_extension(Auth::new())?);
        let service = init_service(create_web_app(app.clone())).await;
        let mut cases = vec![
            (json!({}), false),
            (json!({"enabled": true}), false),
            (json!({"enabled": true, "event": {}}), false),
        ];
        for (field, restricted) in [
            ("ip_whitelist", true),
            ("pubkey_whitelist", true),
            ("event_pubkey_whitelist", true),
            ("ip_blacklist", false),
            ("pubkey_blacklist", true),
            ("event_pubkey_blacklist", false),
        ] {
            for values in [json!([]), json!(["allowed"])] {
                cases.extend([
                    (
                        json!({"enabled": true, "event": {field: values}}),
                        restricted,
                    ),
                    (json!({"enabled": false, "event": {field: values}}), false),
                    (json!({"enabled": true, "req": {field: values}}), false),
                ]);
            }
        }
        for (config, restricted) in cases {
            let mut candidate = app.setting.read().clone();
            candidate.extra.insert("auth".into(), config.clone());
            app.extensions
                .write()
                .call_setting(&app.setting, candidate)?;
            let request = TestRequest::get()
                .uri("/")
                .insert_header(("Accept", "application/nostr+json"))
                .to_request();
            let info: Value = call_and_read_body_json(&service, request).await;
            assert_eq!(
                info["limitation"]["restricted_writes"]
                    .as_bool()
                    .unwrap_or(false),
                restricted,
                "auth config: {config}"
            );
        }
        Ok(())
    }

    #[test]
    fn nip11_preserves_other_write_restrictions() -> Result<()> {
        let setting: SettingWrapper = nostr_relay::Setting::default().into();
        let mut auth = Auth::new();
        setting.write().set_restricted_writes("other", true);
        for enabled in [true, false] {
            setting.write().extra.insert(
                "auth".into(),
                json!({"enabled": enabled, "event": {"ip_whitelist": ["127.0.0.1"]}}),
            );
            auth.setting(&mut setting.write())?();
            let info: Value = serde_json::from_str(&setting.read().render_information()?)?;
            assert_eq!(info["limitation"]["restricted_writes"], json!(true));
        }
        setting.write().set_restricted_writes("other", false);
        let info: Value = serde_json::from_str(&setting.read().render_information()?)?;
        assert!(info["limitation"]["restricted_writes"].is_null());
        Ok(())
    }

    #[test]
    fn empty_pubkey_blacklist_requires_authentication() {
        let permission = Permission {
            pubkey_blacklist: Some(List::default()),
            ..Default::default()
        };
        let pubkey = "writer".to_owned();
        let ip = "127.0.0.1".to_owned();
        assert_eq!(
            Auth::verify_permission(Some(&permission), None, Some(&pubkey), None, &ip),
            Err("NIP-42 auth required")
        );
        assert!(Auth::verify_permission(
            Some(&permission),
            Some(&pubkey),
            Some(&pubkey),
            None,
            &ip,
        )
        .is_ok());
    }

    #[test]
    fn verify() -> Result<()> {
        assert!(Auth::verify_permission(
            Some(&Permission {
                ip_whitelist: Some(vec!["127.0.0.1".to_string()].into()),
                ..Default::default()
            }),
            None,
            None,
            None,
            &"127.0.0.1".to_owned()
        )
        .is_ok());
        assert!(Auth::verify_permission(
            Some(&Permission {
                ip_whitelist: Some(vec!["127.0.0.1".to_string()].into()),
                ..Default::default()
            }),
            None,
            None,
            None,
            &"127.0.0.2".to_owned()
        )
        .is_err());

        assert!(Auth::verify_permission(
            Some(&Permission {
                ip_blacklist: Some(vec!["127.0.0.1".to_string()].into()),
                ..Default::default()
            }),
            None,
            None,
            None,
            &"127.0.0.1".to_owned()
        )
        .is_err());
        assert!(Auth::verify_permission(
            Some(&Permission {
                ip_blacklist: Some(vec!["127.0.0.1".to_string()].into()),
                ..Default::default()
            }),
            None,
            None,
            None,
            &"127.0.0.2".to_owned()
        )
        .is_ok());

        assert!(Auth::verify_permission(
            Some(&Permission {
                pubkey_whitelist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            Some(&"xx".to_owned()),
            None,
            None,
            &"127.0.0.1".to_owned()
        )
        .is_ok());
        assert!(Auth::verify_permission(
            Some(&Permission {
                pubkey_whitelist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            Some(&"xxxx".to_owned()),
            None,
            None,
            &"127.0.0.1".to_owned()
        )
        .is_err());

        assert!(Auth::verify_permission(
            Some(&Permission {
                pubkey_blacklist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            Some(&"xx".to_owned()),
            None,
            None,
            &"127.0.0.1".to_owned()
        )
        .is_err());
        assert!(Auth::verify_permission(
            Some(&Permission {
                pubkey_blacklist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            Some(&"xxxx".to_owned()),
            None,
            None,
            &"127.0.0.1".to_owned()
        )
        .is_ok());

        assert!(Auth::verify_permission(
            Some(&Permission {
                event_pubkey_whitelist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            None,
            Some(&"xx".to_owned()),
            None,
            &"127.0.0.1".to_owned()
        )
        .is_ok());
        assert!(Auth::verify_permission(
            Some(&Permission {
                event_pubkey_whitelist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            None,
            Some(&"xxxx".to_owned()),
            None,
            &"127.0.0.1".to_owned()
        )
        .is_err());

        assert!(Auth::verify_permission(
            Some(&Permission {
                event_pubkey_blacklist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            None,
            Some(&"xx".to_owned()),
            None,
            &"127.0.0.1".to_owned()
        )
        .is_err());
        assert!(Auth::verify_permission(
            Some(&Permission {
                event_pubkey_blacklist: Some(vec!["xx".to_string()].into()),
                ..Default::default()
            }),
            None,
            Some(&"xxxx".to_owned()),
            None,
            &"127.0.0.1".to_owned()
        )
        .is_ok());
        Ok(())
    }

    #[actix_rt::test]
    async fn auth() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = Keypair::new_global(&mut rng);

        let app = create_test_app("auth")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(
                r#"{
                "auth": {
                    "enabled": true
                }
            }"#,
            )?;
        }
        let app = app.add_extension(Auth::new())?;
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        let item = framed.next().await.unwrap()?;
        assert!(matches!(item, ws::Frame::Text(_)));
        let state: (String, String) = parse_text(&item)?;
        assert_eq!(state.0, "AUTH");

        let event = Event::create(&key_pair, 0, 1, vec![], "".to_owned())?;
        let event = Event::new(
            event.id().clone(),
            event.pubkey().clone(),
            event.created_at(),
            2,
            vec![],
            "".to_owned(),
            event.sig().clone(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["AUTH", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.3.contains("invalid"));

        let event = Event::create(&key_pair, now(), 22242, vec![], "".to_owned())?;
        framed
            .send(ws::Message::Text(
                format!(r#"["AUTH", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.3.contains("need"));

        let event = Event::create(
            &key_pair,
            now(),
            22242,
            vec![vec!["challenge".to_owned(), state.1.clone()]],
            "".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["AUTH", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.2);

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));
        Ok(())
    }

    #[actix_rt::test]
    async fn pubkey_whitelist() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = Keypair::new_global(&mut rng);
        let pubkey = XOnlyPublicKey::from_keypair(&key_pair).0;

        let app = create_test_app("auth-whitelist")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(&format!(
                r#"{{
                "auth": {{
                    "enabled": true,
                    "req": {{
                        "pubkey_whitelist": ["{}"]
                    }},
                    "event": {{
                        "pubkey_whitelist": ["{}"]
                    }}
                }}
            }}"#,
                pubkey.to_string(),
                pubkey.to_string()
            ))?;
        }
        let app = app.add_extension(Auth::new())?;
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        let item = framed.next().await.unwrap()?;
        assert!(matches!(item, ws::Frame::Text(_)));
        let state: (String, String) = parse_text(&item)?;
        assert_eq!(state.0, "AUTH");

        // req
        framed
            .send(ws::Message::Text(r#"["REQ", "1", {}]"#.into()))
            .await?;

        let notice: (String, String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(notice.0, "CLOSED");
        assert!(notice.2.contains("auth-required"));

        let event = Event::create(
            &key_pair,
            now(),
            22242,
            vec![vec!["challenge".to_owned(), state.1.clone()]],
            "".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["AUTH", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.2);

        // write
        let event = Event::create(&key_pair, now(), 1, vec![], "test".to_owned())?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.2);

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        let key_pair1 = Keypair::new_global(&mut rng);
        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        let item = framed.next().await.unwrap()?;
        assert!(matches!(item, ws::Frame::Text(_)));
        let state: (String, String) = parse_text(&item)?;
        assert_eq!(state.0, "AUTH");

        let event = Event::create(
            &key_pair1,
            now(),
            22242,
            vec![vec!["challenge".to_owned(), state.1.clone()]],
            "".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["AUTH", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.2);

        // write
        let event = Event::create(&key_pair, now(), 1, vec![], "test".to_owned())?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.3.contains("auth-required"));
        assert!(!notice.2);

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        Ok(())
    }

    #[actix_rt::test]
    async fn nip70() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = Keypair::new_global(&mut rng);

        let app = create_test_app("auth-nip70")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(r#"{ "auth": { "enabled": false } }"#)?;
        }
        let app = app.add_extension(Auth::new())?;
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        // protected event
        let event = Event::create(
            &key_pair,
            now(),
            1,
            vec![vec!["-".to_owned()]],
            "test".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(!notice.2);
        assert!(notice.3.contains("blocked"));

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        Ok(())
    }

    #[actix_rt::test]
    async fn nip70_with_auth() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = Keypair::new_global(&mut rng);
        let key_pair1 = Keypair::new_global(&mut rng);

        // let pubkey = XOnlyPublicKey::from_keypair(&key_pair).0;

        let app = create_test_app("auth-nip70-auth")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(r#"{ "auth": { "enabled": true } }"#)?;
        }
        let app = app.add_extension(Auth::new())?;
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        let item = framed.next().await.unwrap()?;
        assert!(matches!(item, ws::Frame::Text(_)));
        let state: (String, String) = parse_text(&item)?;
        assert_eq!(state.0, "AUTH");

        // protected event without auth
        let event = Event::create(
            &key_pair,
            now(),
            1,
            vec![vec!["-".to_owned()]],
            "test".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(!notice.2);
        assert!(notice.3.contains("authorization"));

        let event = Event::create(
            &key_pair,
            now(),
            22242,
            vec![vec!["challenge".to_owned(), state.1.clone()]],
            "".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["AUTH", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.2);

        let event = Event::create(
            &key_pair1,
            now(),
            1,
            vec![vec!["-".to_owned()]],
            "test".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(!notice.2);
        assert!(notice.3.contains("author"));

        let event = Event::create(
            &key_pair,
            now(),
            1,
            vec![vec!["-".to_owned()]],
            "test".to_owned(),
        )?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.2);

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        Ok(())
    }
}
