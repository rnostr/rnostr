use bytestring::ByteString;
use nostr_db::{Event, Filter};
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::json;
use std::fmt::Display;
use std::{fmt, marker::PhantomData};

/// incoming messages from a client
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

#[derive(Clone, Debug)]
pub enum OutgoingMessage {
    /// message
    Notice(String),
    /// subscription id
    Eose(String),
    /// subscription id, event string
    Event(String, String),
}

impl Display for OutgoingMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutgoingMessage::Notice(notice) => {
                f.write_str(&json!(["NOTICE", notice]).to_string())?;
            }
            OutgoingMessage::Eose(sub_id) => {
                f.write_str(&json!(["EOSE", sub_id]).to_string())?;
            }
            OutgoingMessage::Event(sub_id, event) => {
                f.write_str(&json!(["EVENT", sub_id, event]).to_string())?;
            }
        }
        Ok(())
    }
}

impl Into<ByteString> for OutgoingMessage {
    fn into(self) -> ByteString {
        ByteString::from(self.to_string())
    }
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
        let msg = OutgoingMessage::Notice("hello".to_string());
        let json = msg.to_string();
        assert_eq!(json, r#"["NOTICE","hello"]"#);
        let msg = OutgoingMessage::Event("id".to_string(), "event".to_string());
        let json = msg.to_string();
        assert_eq!(json, r#"["EVENT","id","event"]"#);
        let msg = OutgoingMessage::Eose("hello".to_string());
        let json = msg.to_string();
        assert_eq!(json, r#"["EOSE","hello"]"#);
        Ok(())
    }
}
