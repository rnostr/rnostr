use crate::error::Error;
use nostr_kv::scanner::TimeKey;

// a separator for compare
pub const VIEW_KEY_SEP: [u8; 1] = [0];
// a separator for compare
pub fn concat_sep<K, I>(one: K, two: I) -> Vec<u8>
where
    K: AsRef<[u8]>,
    I: AsRef<[u8]>,
{
    [one.as_ref(), &VIEW_KEY_SEP, two.as_ref()].concat()
}

pub fn concat<K, I>(one: K, two: I) -> Vec<u8>
where
    K: AsRef<[u8]>,
    I: AsRef<[u8]>,
{
    [one.as_ref(), two.as_ref()].concat()
}

pub struct IndexKey {
    time: u64,
    uid: u64,
}

impl IndexKey {
    pub fn encode_time(time: u64) -> Vec<u8> {
        time.to_be_bytes().to_vec()
    }

    pub fn encode_id<K: AsRef<[u8]>>(id: K, time: u64) -> Vec<u8> {
        [id.as_ref(), &time.to_be_bytes()[..]].concat()
    }

    pub fn encode_kind(kind: u16, time: u64) -> Vec<u8> {
        [&kind.to_be_bytes()[..], &time.to_be_bytes()[..]].concat()
    }

    pub fn encode_pubkey<P: AsRef<[u8]>>(pubkey: P, time: u64) -> Vec<u8> {
        [pubkey.as_ref(), &time.to_be_bytes()[..]].concat()
    }

    pub fn encode_pubkey_kind<P: AsRef<[u8]>>(pubkey: P, kind: u16, time: u64) -> Vec<u8> {
        [
            pubkey.as_ref(),
            &kind.to_be_bytes()[..],
            &time.to_be_bytes()[..],
        ]
        .concat()
    }

    pub fn encode_tag<TK: AsRef<[u8]>, TV: AsRef<[u8]>>(
        tag_key: TK,
        tag_val: TV,
        time: u64,
    ) -> Vec<u8> {
        Self::encode_tag1(concat_sep(tag_key, tag_val), time)
    }

    fn encode_tag1<T: AsRef<[u8]>>(tag: T, time: u64) -> Vec<u8> {
        [tag.as_ref(), &VIEW_KEY_SEP, &time.to_be_bytes()[..]].concat()
    }

    pub fn encode_word<P: AsRef<[u8]>>(word: P, time: u64) -> Vec<u8> {
        [word.as_ref(), &VIEW_KEY_SEP, &time.to_be_bytes()[..]].concat()
    }

    pub fn from(key: &[u8], uid: &[u8]) -> Result<Self, Error> {
        let time: u64 = u64::from_be_bytes(key[(key.len() - 8)..].try_into()?);
        let uid: u64 = u64::from_be_bytes(uid[..8].try_into()?);
        Ok(Self { time, uid })
    }

    pub fn uid(&self) -> u64 {
        self.uid
    }
}

impl TimeKey for IndexKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time()
            .cmp(&other.time())
            .then_with(|| self.uid().cmp(&other.uid()))
    }

    fn change_time(&self, key: &[u8], time: u64) -> Vec<u8> {
        let pos = key.len() - 8;
        [&key[0..pos], &time.to_be_bytes()[..]].concat()
    }

    fn time(&self) -> u64 {
        self.time
    }
}

pub fn u64_to_ver(num: u64) -> Vec<u8> {
    num.to_be_bytes().to_vec()
}

pub fn u16_to_ver(num: u16) -> Vec<u8> {
    num.to_be_bytes().to_vec()
}

// Replaceable Events [NIP-16](https://nips.be/16)
// Parameterized Replaceable Events [NIP-33](https://nips.be/33)
pub fn encode_replace_key(kind: u16, pubkey: &[u8; 32], tags: &[Vec<String>]) -> Option<Vec<u8>> {
    if kind == 0 || kind == 3 || kind == 41 || (10_000..20_000).contains(&kind) {
        let k = u16_to_ver(kind);
        let p: &[u8] = pubkey.as_ref();
        Some([p, &k[..]].concat())
    } else if (30_000..40_000).contains(&kind) {
        let k = u16_to_ver(kind);
        let p: &[u8] = pubkey.as_ref();
        let tag = tags
            .get(0)
            .map(|tag| {
                if tag.len() > 1 && tag[0] == "d" {
                    tag.get(1).unwrap().clone()
                } else {
                    "".to_owned()
                }
            })
            .unwrap_or_default();
        Some([p, &k[..], tag.as_bytes()].concat())
    } else {
        None
    }
}
type ReplaceKey<'a> = (&'a [u8], u16, &'a [u8], u64);
#[allow(unused)]
pub fn decode_replace_key<'a>(val: &'a [u8], time: &'a [u8]) -> Result<ReplaceKey<'a>, Error> {
    let len = val.len();
    if len < 32 + 2 {
        Err(Error::InvalidLength)
    } else {
        let pubkey = &val[0..32];
        let kind = u16::from_be_bytes(val[32..34].try_into()?);
        let tag = &val[34..];
        let time = u64::from_be_bytes(time.try_into()?);
        Ok((pubkey, kind, tag, time))
    }
}

// pad '0' at beginning if has not enough length
#[allow(unused)]
pub fn pad_start(id: &Vec<u8>, len: usize) -> Vec<u8> {
    let num = len as i32 - id.len() as i32;
    match num.cmp(&0) {
        std::cmp::Ordering::Less => id[0..len].to_vec(),
        std::cmp::Ordering::Equal => id.clone(),
        std::cmp::Ordering::Greater => {
            let num = num as usize;
            let mut ret = vec![0; len];
            ret[num..].copy_from_slice(id);
            ret
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn pad() {
        assert_eq!(
            pad_start(&vec![1, 2, 3], 32),
            [vec![0u8; 29], vec![1, 2, 3]].concat()
        );
        assert_eq!(pad_start(&vec![1; 33], 32), vec![1; 32]);
        assert_eq!(pad_start(&vec![2; 32], 32), vec![2; 32]);
    }

    #[test]
    fn index_key() -> Result<()> {
        let time = 20u64;
        let id = pad_start(&vec![1, 2, 3], 32);
        let kind = 10u16;
        let pubkey = vec![1; 32];
        let tag_key = "d";
        let tag_val = "m";
        let uid_num = 2u64;
        let uid: Vec<u8> = uid_num.to_be_bytes().to_vec();
        let ind = IndexKey::from(&IndexKey::encode_id(id, time), &uid)?;
        assert_eq!(ind.uid, uid_num);
        assert_eq!(ind.time, time);

        let ind = IndexKey::from(&IndexKey::encode_time(time), &uid)?;
        assert_eq!(ind.uid, uid_num);
        assert_eq!(ind.time, time);

        let ind = IndexKey::from(&IndexKey::encode_kind(kind, time), &uid)?;
        assert_eq!(ind.uid, uid_num);
        assert_eq!(ind.time, time);
        let ind = IndexKey::from(&IndexKey::encode_pubkey(&pubkey, time), &uid)?;
        assert_eq!(ind.uid, uid_num);
        assert_eq!(ind.time, time);

        let ind = IndexKey::from(&IndexKey::encode_pubkey_kind(&pubkey, kind, time), &uid)?;
        assert_eq!(ind.uid, uid_num);
        assert_eq!(ind.time, time);

        let ind = IndexKey::from(&IndexKey::encode_tag(tag_key, tag_val, time), &uid)?;
        assert_eq!(ind.uid, uid_num);
        assert_eq!(ind.time, time);

        Ok(())
    }

    #[test]
    fn replace_key() {
        let tags = vec![vec!["d".to_owned(), "m".to_owned()]];
        let pubkey = [1u8; 32];
        let time = u64_to_ver(10);
        let empty: Vec<u8> = vec![];

        assert!(encode_replace_key(1, &pubkey, &tags).is_none());
        assert!(decode_replace_key(&[1], &time).is_err());

        let k = encode_replace_key(0, &pubkey, &tags).unwrap();
        let r = decode_replace_key(&k, &time).unwrap();
        assert_eq!(r.0, &pubkey);
        assert_eq!(r.1, 0);
        assert_eq!(r.2, empty);
        assert_eq!(r.3, 10);

        let k = encode_replace_key(10001, &pubkey, &tags).unwrap();
        let r = decode_replace_key(&k, &time).unwrap();
        assert_eq!(r.0, &pubkey);
        assert_eq!(r.1, 10001);
        assert_eq!(r.2, empty);
        assert_eq!(r.3, 10);

        let k = encode_replace_key(30001, &pubkey, &tags).unwrap();
        let r = decode_replace_key(&k, &time).unwrap();
        assert_eq!(r.0, &pubkey);
        assert_eq!(r.1, 30001);
        assert_eq!(r.2, "m".as_bytes());
        assert_eq!(r.3, 10);
    }
}
