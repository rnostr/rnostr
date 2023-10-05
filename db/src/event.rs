use crate::error::Error;
use rkyv::{
    vec::ArchivedVec, AlignedVec, Archive, Archived, Deserialize as RkyvDeserialize,
    Serialize as RkyvSerialize,
};
use secp256k1::{schnorr::Signature, KeyPair, Message, XOnlyPublicKey, SECP256K1};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fmt::Display,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

type Tags = Vec<(Vec<u8>, Vec<u8>)>;
type BuildTags = (Tags, Option<u64>, Option<[u8; 32]>);
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
    id: [u8; 32],

    #[serde(with = "hex::serde")]
    pubkey: [u8; 32],

    created_at: u64,

    kind: u16,

    #[serde(skip)]
    tags: Tags,

    #[serde(skip)]
    expiration: Option<u64>,

    /// [NIP-26](https://nips.be/26)
    #[serde(skip)]
    delegator: Option<[u8; 32]>,
}

impl EventIndex {
    pub fn from_zeroes(bytes: &[u8]) -> Result<&ArchivedEventIndex, Error> {
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        Ok(archived)
    }

    pub fn from_bytes<B: AsRef<[u8]>>(bytes: B) -> Result<Self, Error> {
        let bytes = bytes.as_ref();
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        let deserialized: Self = archived
            .deserialize(&mut rkyv::Infallible)
            .map_err(|e| Error::Deserialization(e.to_string()))?;
        Ok(deserialized)
    }

    pub fn to_bytes(&self) -> Result<AlignedVec, Error> {
        let vec =
            rkyv::to_bytes::<_, 256>(self).map_err(|e| Error::Serialization(e.to_string()))?;
        Ok(vec)
    }

    pub fn new(
        id: [u8; 32],
        pubkey: [u8; 32],
        created_at: u64,
        kind: u16,
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

    pub fn build_index_tags(tags: &Vec<Vec<String>>) -> Result<BuildTags, Error> {
        let mut t = vec![];
        let mut expiration = None;
        let mut delegator = None;

        for tag in tags {
            if tag.len() > 1 {
                if tag[0] == "expiration" {
                    expiration = Some(
                        u64::from_str(&tag[1])
                            .map_err(|_| Error::Invalid("invalid expiration".to_string()))?,
                    );
                } else if tag[0] == "delegation" {
                    let mut h = [0u8; 32];
                    hex::decode_to_slice(&tag[1], &mut h)?;
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
                            return Err(Error::Invalid("invalid e or p tag value".to_string()));
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

    pub fn id(&self) -> &[u8; 32] {
        &self.id
    }

    pub fn pubkey(&self) -> &[u8; 32] {
        &self.pubkey
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn kind(&self) -> u16 {
        self.kind
    }

    pub fn tags(&self) -> &Vec<(Vec<u8>, Vec<u8>)> {
        &self.tags
    }

    pub fn expiration(&self) -> Option<&u64> {
        self.expiration.as_ref()
    }

    pub fn delegator(&self) -> Option<&[u8; 32]> {
        self.delegator.as_ref()
    }

    pub fn is_ephemeral(&self) -> bool {
        let kind = self.kind;
        (20_000..30_000).contains(&kind)
    }

    pub fn is_expired(&self, now: u64) -> bool {
        if let Some(exp) = self.expiration {
            exp < now
        } else {
            false
        }
    }
}

impl ArchivedEventIndex {
    pub fn id(&self) -> &Archived<[u8; 32]> {
        &self.id
    }
    pub fn pubkey(&self) -> &Archived<[u8; 32]> {
        &self.pubkey
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn kind(&self) -> u16 {
        self.kind
    }

    pub fn tags(&self) -> &ArchivedVec<(ArchivedVec<u8>, ArchivedVec<u8>)> {
        &self.tags
    }

    pub fn expiration(&self) -> Option<&u64> {
        self.expiration.as_ref()
    }

    pub fn delegator(&self) -> Option<&Archived<[u8; 32]>> {
        self.delegator.as_ref()
    }

    pub fn is_ephemeral(&self) -> bool {
        let kind = self.kind;
        (20_000..30_000).contains(&kind)
    }

    pub fn is_expired(&self, now: u64) -> bool {
        if let Some(exp) = self.expiration.as_ref() {
            exp < &now
        } else {
            false
        }
    }
}
// the shadow event for deserialize
#[derive(Deserialize)]
struct _Event {
    #[serde(with = "hex::serde")]
    id: [u8; 32],
    #[serde(with = "hex::serde")]
    pubkey: [u8; 32],
    created_at: u64,
    kind: u16,
    #[serde(default)]
    tags: Vec<Vec<String>>,
    #[serde(default)]
    content: String,
    #[serde(with = "hex::serde")]
    sig: [u8; 64],
    // #[serde(flatten)]
    // index: IndexEvent,
}

/// The default event document.
// TODO: validate index tag value length 255
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(try_from = "_Event")]
pub struct Event {
    #[serde(default)]
    tags: Vec<Vec<String>>,

    #[serde(default)]
    content: String,

    #[serde(with = "hex::serde")]
    sig: [u8; 64],

    #[serde(flatten)]
    index: EventIndex,

    #[serde(skip)]
    pub words: Vec<Vec<u8>>,
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
            words: Default::default(),
        };
        Ok(event)
    }
}

impl Event {
    pub fn new(
        id: [u8; 32],
        pubkey: [u8; 32],
        created_at: u64,
        kind: u16,
        tags: Vec<Vec<String>>,
        content: String,
        sig: [u8; 64],
    ) -> Result<Self, Error> {
        let index = EventIndex::new(id, pubkey, created_at, kind, &tags)?;
        let event = Self {
            tags,
            content,
            sig,
            index,
            words: Default::default(),
        };
        Ok(event)
    }

    pub fn create(
        key_pair: &KeyPair,
        created_at: u64,
        kind: u16,
        tags: Vec<Vec<String>>,
        content: String,
    ) -> Result<Self, Error> {
        let pubkey = XOnlyPublicKey::from_keypair(key_pair).0.serialize();
        let id = hash(&pubkey, created_at, kind, &tags, &content);
        let sig = *SECP256K1
            .sign_schnorr(&Message::from_slice(&id)?, key_pair)
            .as_ref();
        Self::new(id, pubkey, created_at, kind, tags, content, sig)
    }
}

impl AsRef<Event> for Event {
    fn as_ref(&self) -> &Event {
        self
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
    /// only pass the event id to from_data
    fn only_id() -> bool {
        false
    }
    fn from_data<S: AsRef<[u8]>>(data: S) -> Result<Self, Self::Err>;
}

/// Get the event id
impl FromEventData for Vec<u8> {
    type Err = Error;
    fn only_id() -> bool {
        true
    }
    fn from_data<S: AsRef<[u8]>>(data: S) -> Result<Self, Self::Err> {
        Ok(data.as_ref().to_vec())
    }
}

/// Get the json string
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
                Err(Error::Invalid("Need zstd feature".to_owned()))
            }
        } else {
            Ok(unsafe { String::from_utf8_unchecked(bytes.to_vec()) })
        }
    }
}

fn parse_data_type(json: &[u8]) -> (u8, &[u8]) {
    if !json.is_empty() {
        let last = json.len() - 1;
        let t = json[last];
        if t == 0 || t == 1 {
            return (t, &json[0..last]);
        }
    }
    (0, json)
}

/// Parse the json string to event object
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
                Err(Error::Invalid("Need zstd feature".to_owned()))
            }
        } else {
            Ok(serde_json::from_slice(bytes)?)
        }
    }
}

#[cfg(feature = "search")]
impl Event {
    /// build keywords for search ability
    pub fn build_note_words(&mut self) {
        if self.kind() == 1 {
            let mut words = crate::segment(&self.content);
            self.words.append(&mut words);
        }
    }
}

impl Event {
    /// to json string
    pub fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&self)?)
    }

    pub fn index(&self) -> &EventIndex {
        &self.index
    }

    pub fn id(&self) -> &[u8; 32] {
        &self.index.id
    }

    pub fn id_str(&self) -> String {
        hex::encode(self.index.id)
    }

    pub fn pubkey(&self) -> &[u8; 32] {
        &self.index.pubkey
    }

    pub fn pubkey_str(&self) -> String {
        hex::encode(self.index.pubkey)
    }

    pub fn created_at(&self) -> u64 {
        self.index.created_at
    }

    pub fn kind(&self) -> u16 {
        self.index.kind
    }

    pub fn tags(&self) -> &Vec<Vec<String>> {
        &self.tags
    }

    pub fn content(&self) -> &String {
        &self.content
    }

    pub fn sig(&self) -> &[u8; 64] {
        &self.sig
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hash(
    pubkey: &[u8],
    created_at: u64,
    kind: u16,
    tags: &Vec<Vec<String>>,
    content: &String,
) -> [u8; 32] {
    let json: Value = json!([0, hex::encode(pubkey), created_at, kind, tags, content]);
    let mut hasher = Sha256::new();
    hasher.update(json.to_string());
    hasher.finalize().into()
}

impl Event {
    pub fn hash(&self) -> [u8; 32] {
        hash(
            self.pubkey(),
            self.created_at(),
            self.kind(),
            self.tags(),
            self.content(),
        )
    }

    pub fn verify_id(&self) -> Result<(), Error> {
        if &self.hash() == self.id() {
            Ok(())
        } else {
            Err(Error::Invalid("bad event id".to_owned()))
        }
    }

    pub fn verify_sign(&self) -> Result<(), Error> {
        if verify_sign(&self.sig, self.pubkey(), self.id()).is_ok() {
            Ok(())
        } else {
            Err(Error::Invalid("signature is wrong".to_owned()))
        }
    }

    /// check event created time newer than (now - older), older than (now + newer)
    /// ignore when 0
    pub fn verify_time(&self, now: u64, older: u64, newer: u64) -> Result<(), Error> {
        let time = self.created_at();
        if 0 != older && time < now - older {
            return Err(Error::Invalid(format!(
                "event creation date must be newer than {}",
                now - older
            )));
        }

        if 0 != newer && time > now + newer {
            return Err(Error::Invalid(format!(
                "event creation date must be older than {}",
                now + newer
            )));
        }
        Ok(())
    }

    pub fn verify_delegation(&self) -> Result<(), Error> {
        if self.index.delegator.is_some() {
            for tag in self.tags() {
                if tag.len() == 4 && tag[0] == "delegation" {
                    return verify_delegation(self, &tag[1], &tag[2], &tag[3]);
                }
            }
            Err(Error::Invalid("error delegation arguments".to_owned()))
        } else {
            Ok(())
        }
    }

    pub fn validate(&self, now: u64, older: u64, newer: u64) -> Result<(), Error> {
        if self.index.is_expired(now) {
            return Err(Error::Invalid("event is expired".to_owned()));
        }
        self.verify_time(now, older, newer)?;
        self.verify_id()?;
        self.verify_sign()?;
        self.verify_delegation()?;
        Ok(())
    }
}

fn verify_delegation(
    event: &Event,
    delegator: &String,
    conditions: &String,
    sig: &String,
) -> Result<(), Error> {
    let msg = format!(
        "nostr:delegation:{}:{}",
        hex::encode(event.pubkey()),
        conditions
    );
    let mut hasher = Sha256::new();
    hasher.update(msg);
    let token = hasher.finalize().to_vec();
    verify_sign(&hex::decode(sig)?, &hex::decode(delegator)?, &token)?;
    let time = event.created_at();
    // check conditions
    for cond in conditions.split('&') {
        if let Some(kind) = cond.strip_prefix("kind=") {
            let n = u16::from_str(kind)?;
            if n != event.kind() {
                return Err(Error::Invalid(format!(
                    "event kind must be {}",
                    event.kind()
                )));
            }
        }
        if let Some(t) = cond.strip_prefix("created_at<") {
            let n = u64::from_str(t)?;
            if time >= n {
                return Err(Error::Invalid(format!(
                    "event created_at must older than {}",
                    n
                )));
            }
        }
        if let Some(t) = cond.strip_prefix("created_at>") {
            let n = u64::from_str(t)?;
            if time <= n {
                return Err(Error::Invalid(format!(
                    "event created_at must newer than {}",
                    n
                )));
            }
        }
    }

    Ok(())
}

fn verify_sign(sig: &[u8], pk: &[u8], msg: &[u8]) -> Result<(), Error> {
    let sig = Signature::from_slice(sig)?;
    let pk = XOnlyPublicKey::from_slice(pk)?;
    let msg = Message::from_slice(msg)?;
    Ok(SECP256K1.verify_schnorr(&sig, &msg, &pk)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use secp256k1::rand::thread_rng;
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

        // invalid kind
        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 65536,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"]]
          }
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

    #[test]
    fn verify() -> Result<()> {
        let note = r#"
        {"content":"bgQih8o+R83t00qvueD7twglJRvvabI+nDu+bTvRsAs=?iv=92TlqnpEeiUMzDtUxsZeUA==","created_at":1682257003,"id":"dba1951f0959dfea6e3123ad916d191a07b35392c4b541d4b4814e77113de14a","kind":4,"pubkey":"3f770d65d3a764a9c5cb503ae123e62ec7598ad035d836e2a810f3877a745b24","sig":"15dcc89bca7d037d6a5282c1e63ea40ca4f76d81821ca1260898a324c99516a0cb577617cf18a3febe6303ed32e7a1a08382eecde5a7183195ca8f186a0cb037","tags":[["p","6efb74e66b7ed7fb9fb7b8b8f12e1fbbabe7f45823a33a14ac60cc9241285536"]]}
        "#;
        let event: Event = serde_json::from_str(note)?;
        assert!(event.verify_sign().is_ok());
        assert!(event.verify_id().is_ok());
        assert!(!event.index().is_expired(now()));
        assert!(!event.index().is_ephemeral());

        let note = r#"
        {"content":"{\"display_name\": \"maglevclient\", \"uptime\": 103180, \"maglev\": \"1a98030114cf\"}","created_at":1682258083,"id":"153a480d7bb9d7564147241b330a8667b19c3f9178b8179e64bf57f200654cb0","kind":0,"pubkey":"fb7324a1b807b48756be8df06bd9ccf11741a9678b120e91e044b5137734dcb2","sig":"08c0ffa072fd49f405df467ccab25152a54073fc0639ea0952e1eabff7962e008c54cb8f4d2d55dc4398703df4a5654d2ae3e93f68a801bcbabcdb8050a918ef","tags":[["t","TESTmaglev"],["expiration","1682258683"]]}
          "#;
        let event: Event = serde_json::from_str(note)?;
        assert!(event.verify_sign().is_ok());
        assert!(event.verify_id().is_ok());
        assert!(event.index().is_expired(now()));

        let event = Event::new([0; 32], [0; 32], 10, 1, vec![], "".to_string(), [0; 64])?;
        assert!(event.verify_time(10, 1, 1).is_ok());
        assert!(event.verify_time(20, 1, 1).is_err());
        assert!(event.verify_time(5, 1, 1).is_err());

        let note = r#"
        {
            "id": "e93c6095c3db1c31d15ac771f8fc5fb672f6e52cd25505099f62cd055523224f",
            "pubkey": "477318cfb5427b9cfc66a9fa376150c1ddbc62115ae27cef72417eb959691396",
            "created_at": 1677426298,
            "kind": 1,
            "tags": [
              [
                "delegation",
                "8e0d3d3eb2881ec137a11debe736a9086715a8c8beeeda615780064d68bc25dd",
                "kind=1&created_at>1674834236&created_at<1677426236",
                "6f44d7fe4f1c09f3954640fb58bd12bae8bb8ff4120853c4693106c82e920e2b898f1f9ba9bd65449a987c39c0423426ab7b53910c0c6abfb41b30bc16e5f524"
              ]
            ],
            "content": "Hello, world!",
            "sig": "633db60e2e7082c13a47a6b19d663d45b2a2ebdeaf0b4c35ef83be2738030c54fc7fd56d139652937cdca875ee61b51904a1d0d0588a6acd6168d7be2909d693"
          }
        "#;
        let event: Event = serde_json::from_str(note)?;
        assert!(event.verify_delegation().is_err());
        assert!(event
            .verify_delegation()
            .unwrap_err()
            .to_string()
            .contains("older"));

        Ok(())
    }

    #[test]
    fn create() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = KeyPair::new_global(&mut rng);
        let event = Event::create(&key_pair, 0, 1, vec![], "".to_owned())?;
        assert!(event.verify_sign().is_ok());
        assert!(event.verify_id().is_ok());
        Ok(())
    }
}
