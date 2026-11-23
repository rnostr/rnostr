//! Admission rules for new EVENT writes. Existing events remain readable.

use std::collections::{BTreeMap, BTreeSet};

use nostr_relay::{
    db::{event_identifier, Event, ADDRESSABLE_KINDS},
    message::{ClientMessage, IncomingMessage, OutgoingMessage},
    setting::deserialize_scalar,
    Extension, ExtensionMessageResult, Session, Setting, SettingUpdate,
};
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventPolicySetting {
    #[serde(deserialize_with = "deserialize_scalar")]
    pub enabled: bool,
    pub allowed_kinds: BTreeSet<u16>,
    pub identifiers: BTreeMap<u16, BTreeSet<String>>,
}

impl EventPolicySetting {
    fn parse(setting: &Setting) -> nostr_relay::Result<Self> {
        let parsed: Self = setting.parse_extension("event_policy")?;
        for kind in parsed.identifiers.keys() {
            let problem = if !ADDRESSABLE_KINDS.contains(kind) {
                "must be in 30000..40000"
            } else if !parsed.allowed_kinds.contains(kind) {
                "is missing from allowed_kinds"
            } else {
                continue;
            };
            return Err(nostr_relay::Error::Message(format!(
                "event_policy: identifiers kind {kind} {problem}"
            )));
        }
        Ok(parsed)
    }
}

#[derive(Debug, Default)]
pub struct EventPolicy {
    setting: Option<EventPolicySetting>,
}

impl EventPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    fn check(&self, event: &Event) -> Result<(), &'static str> {
        let Some(setting) = &self.setting else {
            return Err("blocked: event policy configuration is invalid");
        };
        if !setting.enabled {
            return Ok(());
        }
        if !setting.allowed_kinds.contains(&event.kind()) {
            return Err("blocked: event kind is not accepted by this relay");
        }
        if let Some(identifiers) = setting.identifiers.get(&event.kind()) {
            if !identifiers.contains(event_identifier(event.tags())) {
                return Err("blocked: event identifier is not accepted by this relay");
            }
        }
        Ok(())
    }

    fn filter(&self, msg: ClientMessage) -> ExtensionMessageResult {
        if let IncomingMessage::Event(event) = &msg.msg {
            if let Err(reason) = self.check(event) {
                return OutgoingMessage::ok(&event.id_str(), false, reason).into();
            }
        }
        ExtensionMessageResult::Continue(msg)
    }
}

impl Extension for EventPolicy {
    fn name(&self) -> &'static str {
        "event_policy"
    }

    fn setting<'a>(&'a mut self, setting: &mut Setting) -> nostr_relay::Result<SettingUpdate<'a>> {
        let parsed = EventPolicySetting::parse(setting)?;
        setting.set_restricted_writes(self.name(), parsed.enabled);
        Ok(Box::new(move || {
            info!(
                enabled = parsed.enabled,
                allowed_kinds = ?parsed.allowed_kinds,
                "Loaded event write policy"
            );
            self.setting = Some(parsed);
        }))
    }

    fn message(
        &self,
        msg: ClientMessage,
        _session: &mut Session,
        _ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        self.filter(msg)
    }
}

#[cfg(test)]
mod tests;
