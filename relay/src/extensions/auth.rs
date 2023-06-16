use crate::{
    message::{ClientMessage, IncomingMessage, OutgoingMessage},
    setting::SettingWrapper,
    Error, Extension, ExtensionMessageResult, Session,
};
use nostr_db::now;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize, Default, Debug)]
pub struct Permission {
    pub ip_whitelist: Option<Vec<String>>,
    pub pubkey_whitelist: Option<Vec<String>>,
    pub ip_blacklist: Option<Vec<String>>,
    pub pubkey_blacklist: Option<Vec<String>>,
}

#[derive(Deserialize, Default, Debug)]
pub struct AuthSetting {
    pub enabled: bool,
    /// read auth: ["REQ"]
    pub read: Option<Permission>,
    /// write auth: ["EVENT"]
    pub write: Option<Permission>,
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
        Self {
            setting: AuthSetting::default(),
        }
    }

    pub fn verify_permission(
        permission: Option<&Permission>,
        pubkey: Option<&String>,
        ip: &String,
    ) -> Result<(), Error> {
        if let Some(permission) = permission {
            if let Some(list) = &permission.ip_whitelist {
                if !list.contains(ip) {
                    return Err(Error::Message(
                        "restricted: ip not in whitelist".to_string(),
                    ));
                }
            }
            if let Some(list) = &permission.ip_blacklist {
                if list.contains(ip) {
                    return Err(Error::Message("restricted: ip in blacklist".to_string()));
                }
            }
            if let Some(list) = &permission.pubkey_whitelist {
                if let Some(pubkey) = pubkey {
                    if !list.contains(pubkey) {
                        return Err(Error::Message(
                            "restricted: pubkey not in whitelist".to_string(),
                        ));
                    }
                } else {
                    return Err(Error::Message(
                        "restricted: NIP-42 auth required".to_string(),
                    ));
                }
            }
            if let Some(list) = &permission.pubkey_blacklist {
                if let Some(pubkey) = pubkey {
                    if list.contains(pubkey) {
                        return Err(Error::Message(
                            "restricted: pubkey in blacklist".to_string(),
                        ));
                    }
                } else {
                    return Err(Error::Message(
                        "restricted: NIP-42 auth required".to_string(),
                    ));
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
                        } else {
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
                    // write
                    if let Err(err) = Self::verify_permission(
                        self.setting.write.as_ref(),
                        state.map(|s| s.pubkey()).flatten(),
                        session.ip(),
                    ) {
                        return OutgoingMessage::ok(&event.id_str(), false, &err.to_string())
                            .into();
                    }
                }
                IncomingMessage::Req(_) => {
                    // read
                    if let Err(err) = Self::verify_permission(
                        self.setting.read.as_ref(),
                        state.map(|s| s.pubkey()).flatten(),
                        session.ip(),
                    ) {
                        return OutgoingMessage::notice(&err.to_string()).into();
                    }
                }
                _ => {}
            }
        }
        ExtensionMessageResult::Continue(msg)
    }
}
