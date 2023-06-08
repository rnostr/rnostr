use crate::error::Error;
#[cfg(feature = "search")]
use charabia::Segment;
use rkyv::{
    vec::ArchivedVec, AlignedVec, Archive, Deserialize as RkyvDeserialize,
    Serialize as RkyvSerialize,
};
use serde::{Deserialize, Serialize};
use std::{fmt::Display, str::FromStr};

#[derive(
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Debug,
    Clone,
    Default,
    Archive,
    RkyvDeserialize,
    RkyvSerialize,
)]
pub struct EventIndex {
    #[serde(with = "hex::serde")]
    id: Vec<u8>,

    #[serde(with = "hex::serde")]
    pubkey: Vec<u8>,

    created_at: u64,

    kind: u64,

    #[serde(skip)]
    tags: Vec<(Vec<u8>, Vec<u8>)>,

    #[serde(skip)]
    expiration: Option<u64>,

    /// [NIP-26](https://nips.be/26)
    #[serde(skip)]
    delegator: Option<Vec<u8>>,
}

impl EventIndex {
    pub fn from_zeroes<'a>(bytes: &'a [u8]) -> Result<&'a ArchivedEventIndex, Error> {
        let bytes = bytes.as_ref();
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        Ok(archived)
    }

    pub fn from_bytes<B: AsRef<[u8]>>(bytes: B) -> Result<Self, Error> {
        let bytes = bytes.as_ref();
        let archived = unsafe { rkyv::archived_root::<Self>(bytes.as_ref()) };
        let deserialized: Self = archived
            .deserialize(&mut rkyv::Infallible)
            .map_err(|e| Error::Deserialization(e.to_string()))?;
        return Ok(deserialized);
    }

    pub fn to_bytes(&self) -> Result<AlignedVec, Error> {
        let vec =
            rkyv::to_bytes::<_, 256>(self).map_err(|e| Error::Serialization(e.to_string()))?;
        return Ok(vec);
    }

    pub fn new(
        id: Vec<u8>,
        pubkey: Vec<u8>,
        created_at: u64,
        kind: u64,
        tags: &Vec<Vec<String>>,
    ) -> Result<Self, Error> {
        let (tags, expiration, delegator) = Self::build_index_tags(tags)?;
        Ok(Self {
            id,
            pubkey,
            created_at,
            kind,
            tags,
            expiration,
            delegator,
        })
    }

    pub fn build_index_tags(
        tags: &Vec<Vec<String>>,
    ) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, Option<u64>, Option<Vec<u8>>), Error> {
        let mut t = vec![];
        let mut expiration = None;
        let mut delegator = None;

        for tag in tags {
            if tag.len() > 1 {
                if tag[0] == "expiration" {
                    expiration = Some(
                        u64::from_str(&tag[1])
                            .map_err(|_| Error::Invald("invalid expiration".to_string()))?,
                    );
                } else if tag[0] == "delegation" {
                    let h = hex::decode(&tag[1])?;
                    if h.len() != 32 {
                        return Err(Error::Invald("invalid delegator length".to_string()));
                    }
                    delegator = Some(h);
                }

                let key = tag[0].as_bytes().to_vec();
                // only index key length 1
                // 0 will break the index separator, ignore
                if key.len() == 1 && key[0] != 0 {
                    let v;
                    // fixed length 32 e and p
                    if tag[0] == "e" || tag[0] == "p" {
                        let h = hex::decode(&tag[1])?;
                        if h.len() != 32 {
                            return Err(Error::Invald("invalid e or p tag value".to_string()));
                        }
                        v = h;
                    } else {
                        v = tag[1].as_bytes().to_vec();
                        // 0 will break the index separator, ignore
                        // lmdb max_key_size 511 bytes
                        // we only index tag value length < 255
                        if v.contains(&0) || v.len() > 255 {
                            continue;
                        }
                    };
                    t.push((key, v));
                }
            }
        }
        Ok((t, expiration, delegator))
    }

    pub fn id(&self) -> &Vec<u8> {
        &self.id
    }

    pub fn pubkey(&self) -> &Vec<u8> {
        &self.pubkey
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn kind(&self) -> u64 {
        self.kind
    }

    pub fn tags(&self) -> &Vec<(Vec<u8>, Vec<u8>)> {
        &self.tags
    }

    pub fn expiration(&self) -> Option<&u64> {
        return self.expiration.as_ref();
    }

    pub fn delegator(&self) -> Option<&Vec<u8>> {
        return self.delegator.as_ref();
    }
}

impl ArchivedEventIndex {
    pub fn id(&self) -> &[u8] {
        &self.id
    }
    pub fn pubkey(&self) -> &[u8] {
        &self.pubkey
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn kind(&self) -> u64 {
        self.kind
    }

    pub fn tags(&self) -> &ArchivedVec<(ArchivedVec<u8>, ArchivedVec<u8>)> {
        &self.tags
    }

    pub fn expiration(&self) -> Option<&u64> {
        return self.expiration.as_ref();
    }

    pub fn delegator(&self) -> Option<&ArchivedVec<u8>> {
        return self.delegator.as_ref();
    }
}
// the shadow event for deserialize
#[derive(Deserialize, Default)]
struct _Event {
    #[serde(with = "hex::serde")]
    id: Vec<u8>,
    #[serde(with = "hex::serde")]
    pubkey: Vec<u8>,
    created_at: u64,
    kind: u64,
    #[serde(default)]
    tags: Vec<Vec<String>>,
    #[serde(default)]
    content: String,
    #[serde(with = "hex::serde")]
    sig: Vec<u8>,
    // #[serde(flatten)]
    // index: IndexEvent,
}

/// The default event document.
// TODO: validate index tag value length 255
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(try_from = "_Event")]
pub struct Event {
    #[serde(default)]
    tags: Vec<Vec<String>>,

    #[serde(default)]
    content: String,

    #[serde(with = "hex::serde")]
    sig: Vec<u8>,

    #[serde(flatten)]
    index: EventIndex,

    #[serde(skip)]
    pub words: Option<Vec<Vec<u8>>>,
}

impl TryFrom<_Event> for Event {
    type Error = Error;

    fn try_from(value: _Event) -> Result<Self, Self::Error> {
        let event = Event {
            content: value.content,
            sig: value.sig,
            index: EventIndex::new(
                value.id,
                value.pubkey,
                value.created_at,
                value.kind,
                &value.tags,
            )?,
            tags: value.tags,
            words: None,
        };
        Ok(event)
    }
}

impl Event {
    pub fn new(
        id: Vec<u8>,
        pubkey: Vec<u8>,
        created_at: u64,
        kind: u64,
        tags: Vec<Vec<String>>,
        content: String,
        sig: Vec<u8>,
    ) -> Result<Self, Error> {
        let index = EventIndex::new(id, pubkey, created_at, kind, &tags)?;
        let event = Self {
            tags,
            content,
            sig,
            index,
            words: None,
        };
        Ok(event)
    }
}

impl AsRef<Event> for Event {
    fn as_ref(&self) -> &Event {
        &self
    }
}

impl FromStr for Event {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(serde_json::from_str(s)?)
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = serde_json::to_string(&self).unwrap();
        f.write_str(&str)?;
        Ok(())
    }
}

impl TryInto<String> for Event {
    type Error = Error;
    fn try_into(self) -> Result<String, Self::Error> {
        Ok(serde_json::to_string(&self)?)
    }
}

pub trait FromEventData: Sized {
    type Err: std::error::Error;
    fn from_data<S: AsRef<[u8]>>(json: S) -> Result<Self, Self::Err>;
}

impl FromEventData for String {
    type Err = Error;
    fn from_data<S: AsRef<[u8]>>(json: S) -> Result<Self, Self::Err> {
        let (t, bytes) = parse_data_type(json.as_ref());
        if t == 1 {
            #[cfg(feature = "zstd")]
            {
                let bytes = zstd::decode_all(bytes)?;
                Ok(unsafe { String::from_utf8_unchecked(bytes) })
            }
            #[cfg(not(feature = "zstd"))]
            {
                Err(Error::Invald("Need zstd feature".to_owned()))
            }
        } else {
            Ok(unsafe { String::from_utf8_unchecked(bytes.to_vec()) })
        }
    }
}

fn parse_data_type(json: &[u8]) -> (u8, &[u8]) {
    if json.len() > 0 {
        let last = json.len() - 1;
        let t = json[last];
        if t == 0 || t == 1 {
            return (t, &json[0..last]);
        }
    }
    (0, json)
}

impl FromEventData for Event {
    type Err = Error;
    /// decode the json data to event object
    fn from_data<S: AsRef<[u8]>>(json: S) -> Result<Self, Self::Err> {
        let (t, bytes) = parse_data_type(json.as_ref());
        if t == 1 {
            #[cfg(feature = "zstd")]
            {
                let bytes = zstd::decode_all(bytes)?;
                Ok(serde_json::from_slice(&bytes)?)
            }
            #[cfg(not(feature = "zstd"))]
            {
                Err(Error::Invald("Need zstd feature".to_owned()))
            }
        } else {
            Ok(serde_json::from_slice(bytes)?)
        }
    }
}

impl Event {
    /// to json string
    pub fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&self)?)
    }

    #[cfg(feature = "search")]
    /// build keywords for search ability
    pub fn build_words(&mut self) {
        if self.kind() == 1 {
            let s: &str = self.content.as_ref();
            let iter = s.segment_str();
            let vec = iter
                .filter_map(|s| {
                    let bytes = s.as_bytes();
                    // limit size
                    if bytes.len() < 255 {
                        Some(bytes.to_vec())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            if vec.len() > 0 {
                self.words = Some(vec);
            }
        }
    }

    pub fn index(&self) -> &EventIndex {
        &self.index
    }

    pub fn id(&self) -> &Vec<u8> {
        &self.index.id
    }

    pub fn pubkey(&self) -> &Vec<u8> {
        &self.index.pubkey
    }

    pub fn created_at(&self) -> u64 {
        self.index.created_at
    }

    pub fn kind(&self) -> u64 {
        self.index.kind
    }

    pub fn tags(&self) -> &Vec<Vec<String>> {
        &self.tags
    }

    pub fn content(&self) -> &String {
        &self.content
    }
}

#[cfg(test)]
mod tests {
    use crate::EventIndex;

    use super::Event;
    use anyhow::Result;
    use serde_json::Value;
    use std::str::FromStr;

    #[test]
    fn index_event() -> Result<()> {
        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"], ["t", ""], ["expiration", "1"], ["delegation", "8e0d3d3eb2881ec137a11debe736a9086715a8c8beeeda615780064d68bc25dd"]]
          }
        "#;
        let event: Event = Event::from_str(note)?;
        assert_eq!(event.index().tags().len(), 2);
        let e2 = EventIndex::from_bytes(&event.index().to_bytes()?)?;
        assert_eq!(&e2, event.index());
        assert!(&e2.expiration().is_some());
        assert!(&e2.delegator().is_some());

        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": []
          }
        "#;
        let event: Event = Event::from_str(note)?;
        assert_eq!(event.index().tags().len(), 0);
        let e2 = EventIndex::from_bytes(&event.index().to_bytes()?)?;
        assert_eq!(&e2, event.index());
        Ok(())
    }

    #[test]
    fn string() -> Result<()> {
        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"]]
          }
        "#;
        let event: Event = Event::from_str(note)?;
        assert_eq!(
            hex::encode(event.index().id()),
            "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d"
        );
        assert_eq!(
            hex::encode(event.index().id()),
            "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d"
        );
        assert_eq!(event.index().id().len(), 32);
        let json: String = event.try_into()?;
        let val: Value = serde_json::from_str(&json)?;
        assert_eq!(
            val["id"],
            Value::String(
                "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d".to_string()
            )
        );
        Ok(())
    }
    #[test]
    fn deserialize() -> Result<()> {
        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"]]
          }
        "#;
        let event: Event = serde_json::from_str(note)?;
        assert_eq!(
            hex::encode(event.index().id()),
            "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d"
        );
        assert_eq!(
            hex::encode(event.index().id()),
            "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d"
        );
        assert_eq!(event.index().id().len(), 32);
        assert_eq!(&event.tags, &vec![vec!["t", "nostr"]]);
        assert_eq!(event.index().tags.len(), 1);

        // null tag
        let note = r#"
        {"content":"","created_at":1681838474,"id":"bf2b783de44b814778d02ca9e4e87aacd0bc7a629bad29b5db62a1c151580ed1","kind":1,"pubkey":"d477a41316e6d28c469181690237705024eb313b43ed3e1f059dc2ff49a6dd2f","sig":"96fa5e33aefd4b18f2d5ab5dc199e731fd6c33162ef3eeee945959b98901e80d1b8fb62856f4f0baed166f4aab2d4401aa8ce9e48071dbe220d2b8e9773755de","tags":[["e","fad5161223be749e364f0eac0fc8cf1566659a32c75d9ce388be42c36ac33e44",null,"root"]]}
        "#;
        let event = Event::from_str(note);
        assert!(event.is_err());

        Ok(())
    }

    #[test]
    fn default() -> Result<()> {
        let note = r#"
        {
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f"
          }
        "#;
        let event: Event = serde_json::from_str(note)?;
        assert_eq!(&event.content, "");
        assert_eq!(&event.tags, &Vec::<Vec<String>>::new());
        Ok(())
    }
}
