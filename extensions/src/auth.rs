use metrics::{describe_counter, increment_counter};
use nostr_relay::db::now;
use nostr_relay::{
    message::{ClientMessage, IncomingMessage, OutgoingMessage},
    setting::SettingWrapper,
    Extension, ExtensionMessageResult, List, Session,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Permission {
    pub ip_whitelist: Option<List>,
    pub pubkey_whitelist: Option<List>,
    pub ip_blacklist: Option<List>,
    pub pubkey_blacklist: Option<List>,
    pub event_pubkey_whitelist: Option<List>,
    pub event_pubkey_blacklist: Option<List>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct AuthSetting {
    pub enabled: bool,
    /// read auth: ["REQ"]
    pub req: Option<Permission>,
    /// write auth: ["EVENT"]
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
                    if !list.contains(pubkey) {
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

    fn setting(&mut self, setting: &SettingWrapper) {
        let mut w = setting.write();
        self.setting = w.parse_extension(self.name());
        if self.setting.enabled {
            w.add_nip(42);
        }
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
        if self.setting.enabled {
            let state = session.get::<AuthState>();
            match &msg.msg {
                IncomingMessage::Auth(event) => {
                    if let Some(AuthState::Challenge(challenge)) = state {
                        if let Err(err) = event.validate(now(), 0, 0) {
                            return OutgoingMessage::notice(&err.to_string()).into();
                        } else if event.kind() == 22242 {
                            for tag in event.tags() {
                                if tag.len() > 1 && tag[0] == "challenge" && &tag[1] == challenge {
                                    session.set(AuthState::Pubkey(event.pubkey_str()));
                                    return OutgoingMessage::notice("auth success").into();
                                }
                            }
                        }
                    }
                    return OutgoingMessage::notice("auth error").into();
                }
                IncomingMessage::Event(event) => {
                    if let Err(err) = Self::verify_permission(
                        self.setting.event.as_ref(),
                        state.and_then(|s| s.pubkey()),
                        Some(&event.pubkey_str()),
                        session.ip(),
                    ) {
                        increment_counter!("nostr_relay_auth_unauthorized", "command" => "EVENT", "reason" => err);
                        return OutgoingMessage::ok(
                            &event.id_str(),
                            false,
                            &format!("restricted: {}", err),
                        )
                        .into();
                    }
                }
                IncomingMessage::Req(_) => {
                    if let Err(err) = Self::verify_permission(
                        self.setting.req.as_ref(),
                        state.and_then(|s| s.pubkey()),
                        None,
                        session.ip(),
                    ) {
                        increment_counter!("nostr_relay_auth_unauthorized", "command" => "REQ", "reason" => err);
                        return OutgoingMessage::notice(&format!("restricted: {}", err)).into();
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
    use actix_web::web;
    use actix_web_actors::ws;
    use anyhow::Result;
    use futures_util::{SinkExt as _, StreamExt as _};
    use nostr_relay::create_web_app;
    use nostr_relay::db::{
        secp256k1::{rand::thread_rng, KeyPair, XOnlyPublicKey},
        Event,
    };

    fn parse_text<T: serde::de::DeserializeOwned>(frame: &ws::Frame) -> Result<T> {
        if let ws::Frame::Text(text) = &frame {
            let data: T = serde_json::from_slice(text)?;
            Ok(data)
        } else {
            Err(nostr_relay::Error::Message("invalid frame type".to_string()).into())
        }
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
            &"127.0.0.1".to_owned()
        )
        .is_ok());
        Ok(())
    }

    #[actix_rt::test]
    async fn auth() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = KeyPair::new_global(&mut rng);

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
        let app = app.add_extension(Auth::new());
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
        let notice: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.1.contains("invalid"));

        let event = Event::create(&key_pair, now(), 22242, vec![], "".to_owned())?;
        framed
            .send(ws::Message::Text(
                format!(r#"["AUTH", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.1.contains("error"));

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
        let notice: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.1.contains("success"));

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
        let key_pair = KeyPair::new_global(&mut rng);
        let pubkey = XOnlyPublicKey::from_keypair(&key_pair).0;

        let app = create_test_app("auth-whitelist")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(&format!(
                r#"{{
                "auth": {{
                    "enabled": true,
                    "event": {{
                        "pubkey_whitelist": ["{}"]
                    }}
                }}
            }}"#,
                pubkey.to_string()
            ))?;
        }
        let app = app.add_extension(Auth::new());
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        let item = framed.next().await.unwrap()?;
        assert!(matches!(item, ws::Frame::Text(_)));
        let state: (String, String) = parse_text(&item)?;
        assert_eq!(state.0, "AUTH");

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
        let notice: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.1.contains("success"));

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

        let key_pair1 = KeyPair::new_global(&mut rng);
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
        let notice: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.1.contains("success"));

        // write
        let event = Event::create(&key_pair, now(), 1, vec![], "test".to_owned())?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(notice.3.contains("restricted"));
        assert!(!notice.2);

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        Ok(())
    }
}
