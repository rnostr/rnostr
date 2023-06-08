use actix::{Message, MessageResponse, Recipient};
use bytestring::ByteString;
use nostr_db::{CheckEventResult, Event, Filter};
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::json;
use std::fmt::Display;
use std::{fmt, marker::PhantomData};

/// New session is created
#[derive(Message)]
#[rtype(usize)]
pub struct Connect {
    pub addr: Recipient<OutgoingMessage>,
}

/// Session is disconnected
#[derive(Message)]
#[rtype(result = "()")]
pub struct Disconnect {
    pub id: usize,
}

/// Message from client
#[derive(Message)]
#[rtype(result = "()")]
pub struct ClientMessage {
    /// Id of the client session
    pub id: usize,
    /// text message
    pub text: String,
    /// parsed message
    pub msg: IncomingMessage,
}

/// Parsed incoming messages from a client
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "UPPERCASE", tag = "0")]
pub enum IncomingMessage {
    Event {
        event: Event,
    },
    Close {
        id: String,
    },
    Req(Subscription),
    #[serde(other, deserialize_with = "ignore_contents")]
    Unknown,
}

fn ignore_contents<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: Deserializer<'de>,
{
    // Ignore any content at this part of the json structure
    let _ = deserializer.deserialize_ignored_any(serde::de::IgnoredAny);
    // Return unit as our 'Unknown' variant has no args
    Ok(())
}

/// Subscription
#[derive(Clone, Debug)]
pub struct Subscription {
    pub id: String,
    pub filters: Vec<Filter>,
}

// https://github.com/serde-rs/serde/issues/1337
// prefix
impl<'de> Deserialize<'de> for Subscription {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PrefixVisitor(PhantomData<()>);

        impl<'de> Visitor<'de> for PrefixVisitor {
            type Value = Subscription;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("sequence")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let t = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let r = Vec::<Filter>::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                Ok(Subscription { id: t, filters: r })
            }
        }

        deserializer.deserialize_seq(PrefixVisitor(PhantomData))
    }
}

/// The message sent to the client, the first parameter is the message content,
/// if the second parameter event is provided, then the first parameter is the subscription id.
/// event to string has some time consumption, it is not desirable to convert this message when it is generated.
#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct OutgoingMessage(pub String, pub Option<Event>);

impl OutgoingMessage {
    pub fn notice(message: &str) -> Self {
        Self(json!(["NOTICE", message]).to_string(), None)
    }

    pub fn eose(sub_id: &str) -> Self {
        Self(format!(r#"["EOSE","{}"]"#, sub_id), None)
    }

    pub fn event(sub_id: &str, event: &str) -> Self {
        Self(format!(r#"["EVENT","{}",{}]"#, sub_id, event), None)
    }

    pub fn ok(event_id: &str, saved: bool, message: &str) -> Self {
        Self(json!(["Ok", event_id, saved, message]).to_string(), None)
    }
}

impl Display for OutgoingMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(event) = &self.1 {
            let msg = Self::event(&self.0, &event.to_string());
            f.write_str(&msg.0)?;
        } else {
            f.write_str(&self.0)?;
        }
        Ok(())
    }
}

impl Into<ByteString> for OutgoingMessage {
    fn into(self) -> ByteString {
        if let Some(event) = &self.1 {
            let msg = Self::event(&self.0, &event.to_string());
            ByteString::from(msg.0)
        } else {
            ByteString::from(self.0)
        }
    }
}

#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct WriteEvent {
    pub id: usize,
    pub event: Event,
}

#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub enum WriteEventResult {
    Write {
        id: usize,
        event: Event,
        result: CheckEventResult,
    },
    Message {
        id: usize,
        event: Event,
        msg: OutgoingMessage,
    },
}
// pub struct WriteEventResult {
//     pub id: usize,
//     pub event: Event,
//     pub result: CheckEventResult,
// }

#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct ReadEvent {
    pub id: usize,
    pub subscription: Subscription,
}

#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct ReadEventResult {
    pub id: usize,
    pub sub_id: String,
    pub msg: OutgoingMessage,
}

#[derive(MessageResponse, Clone, Debug, PartialEq, Eq)]
pub enum Subscribed {
    Ok,
    Overlimit,
    Duplicate,
}

#[derive(Message, Clone, Debug)]
#[rtype(result = "Subscribed")]
pub struct Subscribe {
    pub id: usize,
    pub subscription: Subscription,
}

#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct Unsubscribe {
    pub id: usize,
    pub sub_id: Option<String>,
}

#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct Dispatch {
    pub id: usize,
    pub event: Event,
}

#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct SubscribeResult {
    pub id: usize,
    pub sub_id: String,
    pub msg: OutgoingMessage,
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn de_incoming_message() -> Result<()> {
        // close
        let msg: IncomingMessage = serde_json::from_str(r#"["CLOSE", "sub_id1"]"#)?;
        assert!(matches!(msg, IncomingMessage::Close { ref id } if id == "sub_id1"));

        let msg = serde_json::from_str::<IncomingMessage>(r#"["CLOSE", "sub_id1", "other"]"#);
        assert!(msg.is_err());

        // event
        let msg: IncomingMessage = serde_json::from_str(
            r#"["EVENT", {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"], ["t", ""], ["expiration", "1"], ["delegation", "8e0d3d3eb2881ec137a11debe736a9086715a8c8beeeda615780064d68bc25dd"]]
          }]"#,
        )?;
        assert!(matches!(msg, IncomingMessage::Event { ref event } if event.kind() == 1));

        let sub: Subscription = serde_json::from_str(r#"["sub_id1", {}, {}]"#)?;
        assert_eq!(sub.id, "sub_id1");
        assert_eq!(sub.filters.len(), 2);

        // req
        let msg: IncomingMessage = serde_json::from_str(r#"["REQ", "sub_id1", {}]"#)?;
        assert!(matches!(msg, IncomingMessage::Req(sub) if sub.id == "sub_id1"));

        // unknown
        let msg: IncomingMessage = serde_json::from_str(r#"["REQ1", "sub_id1", {}]"#)?;
        assert!(matches!(msg, IncomingMessage::Unknown));

        Ok(())
    }

    #[test]
    fn se_outgoing_message() -> Result<()> {
        let msg = OutgoingMessage::notice("hello");
        let json = msg.to_string();
        assert_eq!(json, r#"["NOTICE","hello"]"#);
        let msg = OutgoingMessage::event("id", r#"{"id":"1"}"#);
        let json = msg.to_string();
        assert_eq!(json, r#"["EVENT","id",{"id":"1"}]"#);
        let msg = OutgoingMessage::eose("hello");
        let json = msg.to_string();
        assert_eq!(json, r#"["EOSE","hello"]"#);
        let event = Event::default();
        let msg = OutgoingMessage("id".to_owned(), Some(event));
        let json = msg.to_string();
        assert!(json.starts_with(r#"["EVENT","id",{"#));
        Ok(())
    }
}
