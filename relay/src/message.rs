use actix::{Message, MessageResponse, Recipient};
use bytestring::ByteString;
use nostr_db::{now, CheckEventResult, Event, Filter};
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{json, Value};
use std::fmt::Display;
use std::{fmt, marker::PhantomData};

use crate::{setting::Limitation, Error};

/// New session is created
#[derive(Message, Clone, Debug)]
#[rtype(usize)]
pub struct Connect {
    pub addr: Recipient<OutgoingMessage>,
}

/// Session is disconnected
#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct Disconnect {
    pub id: usize,
}

/// Message from client
#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct ClientMessage {
    /// Id of the client session
    pub id: usize,
    /// text message
    pub text: String,
    /// parsed message
    pub msg: IncomingMessage,
}

macro_rules! check_max {
    ($source:expr, $limit:expr) => {
        if $source > $limit {
            return Err(Error::Invalid(format!("{} {}", stringify!($limit), $limit)));
        }
    };
}

macro_rules! check_min {
    ($source:expr, $limit:expr) => {
        if $source < $limit {
            return Err(Error::Invalid(format!("{} {}", stringify!($limit), $limit)));
        }
    };
}

impl ClientMessage {
    pub fn validate(&mut self, limitation: &Limitation) -> Result<(), Error> {
        check_max!(self.text.as_bytes().len(), limitation.max_message_length);

        match &mut self.msg {
            IncomingMessage::Event(event) => {
                check_max!(event.tags().len(), limitation.max_event_tags);
                event.validate(
                    now(),
                    limitation.max_event_time_older_than_now,
                    limitation.max_event_time_newer_than_now,
                )?;
            }

            IncomingMessage::Req(sub) => {
                check_max!(sub.filters.len(), limitation.max_filters);
                check_max!(sub.id.len(), limitation.max_subid_length);

                for f in &mut sub.filters {
                    // fill default limit
                    f.default_limit(limitation.max_limit);
                    check_max!(f.limit.unwrap(), limitation.max_limit);
                    for id in f.ids.iter() {
                        check_min!(id.len(), limitation.min_prefix);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

// #[derive(Deserialize, Clone, Debug)]
// #[serde(rename_all = "UPPERCASE", tag = "0")]
// pub enum IncomingMessage {
//     Event {
//         event: Event,
//     },
//     Close {
//         id: String,
//     },
//     Req(Subscription),
//     #[serde(other, deserialize_with = "ignore_contents")]
//     Unknown,
// }

/// Parsed incoming messages from a client
#[derive(Clone, Debug)]
pub enum IncomingMessage {
    Event(Event),
    Close(String),
    Req(Subscription),
    /// nip-42
    Auth(Event),
    /// nip-45
    Count(Subscription),
    Unknown(String, Vec<Value>),
}

impl IncomingMessage {
    pub fn command(&self) -> &str {
        match self {
            IncomingMessage::Event(_) => "EVENT",
            IncomingMessage::Close(_) => "CLOSE",
            IncomingMessage::Req(_) => "REQ",
            IncomingMessage::Auth(_) => "AUTH",
            IncomingMessage::Count(_) => "COUNT",
            IncomingMessage::Unknown(cmd, _) => cmd,
        }
    }

    pub fn known_command(&self) -> Option<&'static str> {
        match self {
            IncomingMessage::Event(_) => Some("EVENT"),
            IncomingMessage::Close(_) => Some("CLOSE"),
            IncomingMessage::Req(_) => Some("REQ"),
            IncomingMessage::Auth(_) => Some("AUTH"),
            IncomingMessage::Count(_) => Some("COUNT"),
            IncomingMessage::Unknown(_, _) => None,
        }
    }
}

// https://github.com/serde-rs/serde/issues/1337

struct MessageVisitor(PhantomData<()>);

impl<'de> Visitor<'de> for MessageVisitor {
    type Value = IncomingMessage;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("sequence")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let t: &str = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        match t {
            "EVENT" => Ok(IncomingMessage::Event(
                seq.next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?,
            )),
            "CLOSE" => Ok(IncomingMessage::Close(
                seq.next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?,
            )),
            "REQ" => {
                let t = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let r = Vec::<Filter>::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                Ok(IncomingMessage::Req(Subscription { id: t, filters: r }))
            }
            "AUTH" => Ok(IncomingMessage::Auth(
                seq.next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?,
            )),
            "COUNT" => {
                let t = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let r = Vec::<Filter>::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                Ok(IncomingMessage::Count(Subscription { id: t, filters: r }))
            }
            _ => Ok(IncomingMessage::Unknown(
                t.to_string(),
                Vec::<Value>::deserialize(de::value::SeqAccessDeserializer::new(seq))?,
            )),
        }
    }
}

impl<'de> Deserialize<'de> for IncomingMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(MessageVisitor(PhantomData))
    }
}

// fn ignore_contents<'de, D>(deserializer: D) -> Result<(), D::Error>
// where
//     D: Deserializer<'de>,
// {
//     // Ignore any content at this part of the json structure
//     let _ = deserializer.deserialize_ignored_any(serde::de::IgnoredAny);
//     // Return unit as our 'Unknown' variant has no args
//     Ok(())
// }

/// Subscription
#[derive(Clone, Debug)]
pub struct Subscription {
    pub id: String,
    pub filters: Vec<Filter>,
}

// https://github.com/serde-rs/serde/issues/1337
// prefix
// impl<'de> Deserialize<'de> for Subscription {
//     fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
//     where
//         D: Deserializer<'de>,
//     {
//         struct PrefixVisitor(PhantomData<()>);

//         impl<'de> Visitor<'de> for PrefixVisitor {
//             type Value = Subscription;

//             fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
//                 formatter.write_str("sequence")
//             }

//             fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
//             where
//                 A: SeqAccess<'de>,
//             {
//                 let t = seq
//                     .next_element()?
//                     .ok_or_else(|| de::Error::invalid_length(0, &self))?;
//                 let r = Vec::<Filter>::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
//                 Ok(Subscription { id: t, filters: r })
//             }
//         }

//         deserializer.deserialize_seq(PrefixVisitor(PhantomData))
//     }
// }

/// The message sent to the client
#[derive(Message, Clone, Debug)]
#[rtype(result = "()")]
pub struct OutgoingMessage(pub String);

impl OutgoingMessage {
    pub fn notice(message: &str) -> Self {
        Self(json!(["NOTICE", message]).to_string())
    }

    pub fn eose(sub_id: &str) -> Self {
        Self(format!(r#"["EOSE","{}"]"#, sub_id))
    }

    pub fn event(sub_id: &str, event: &str) -> Self {
        Self(format!(r#"["EVENT","{}",{}]"#, sub_id, event))
    }

    pub fn ok(event_id: &str, saved: bool, message: &str) -> Self {
        Self(json!(["OK", event_id, saved, message]).to_string())
    }
}

impl Display for OutgoingMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)?;
        Ok(())
    }
}

// impl Into<ByteString> for OutgoingMessage {
//     fn into(self) -> ByteString {
//         ByteString::from(self.0)
//     }
// }

impl From<OutgoingMessage> for ByteString {
    fn from(val: OutgoingMessage) -> Self {
        ByteString::from(val.0)
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
    InvalidIdLength,
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
        assert!(matches!(msg, IncomingMessage::Close(ref id) if id == "sub_id1"));

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
        assert!(matches!(msg, IncomingMessage::Event( ref event ) if event.kind() == 1));

        // let sub: Subscription = serde_json::from_str(r#"["sub_id1", {}, {}]"#)?;
        // assert_eq!(sub.id, "sub_id1");
        // assert_eq!(sub.filters.len(), 2);

        // req
        let msg: IncomingMessage = serde_json::from_str(r#"["REQ", "sub_id1", {}]"#)?;
        assert!(matches!(msg, IncomingMessage::Req(sub) if sub.id == "sub_id1"));
        let msg = serde_json::from_str::<IncomingMessage>(r#"["REQ", "sub_id1", ""]"#);
        assert!(msg.is_err());
        let msg = serde_json::from_str::<IncomingMessage>(r#"["REQ", "sub_id1"]"#);
        assert!(msg.is_ok());

        // unknown
        let msg: IncomingMessage = serde_json::from_str(r#"["REQ1", "sub_id1", {}]"#)?;
        assert!(matches!(msg, IncomingMessage::Unknown(ref cmd, ref _val) if cmd == "REQ1"));

        // auth
        let msg: IncomingMessage = serde_json::from_str(
            r#"["AUTH", {
    "content": "Good morning everyone 😃",
    "created_at": 1680690006,
    "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
    "kind": 1,
    "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
    "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
    "tags": [["t", "nostr"], ["t", ""], ["expiration", "1"], ["delegation", "8e0d3d3eb2881ec137a11debe736a9086715a8c8beeeda615780064d68bc25dd"]]
  }]"#,
        )?;
        assert!(matches!(msg, IncomingMessage::Auth( ref event ) if event.kind() == 1));

        // count
        let msg: IncomingMessage = serde_json::from_str(r#"["COUNT", "sub_id1", {}]"#)?;
        assert!(matches!(msg, IncomingMessage::Count(sub) if sub.id == "sub_id1"));

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
        // let event = Event::default();
        // let msg = OutgoingMessage("id".to_owned(), Some(event));
        // let json = msg.to_string();
        // assert!(json.starts_with(r#"["EVENT","id",{"#));
        Ok(())
    }
}
