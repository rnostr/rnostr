use anyhow::Result;
use nostr_kv::{
    lmdb::{ffi, Db, Transaction},
    scanner::*,
    Error,
};
use std::ops::Bound;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct Key<'txn> {
    k: &'txn [u8],
    v: &'txn [u8],
    // uid: Vec<u8>,
    // kind: u64,
    // time: u64,
}

impl<'txn> Key<'txn> {
    fn encode(kind: u64, time: u64) -> Vec<u8> {
        [&kind.to_be_bytes()[..], &time.to_be_bytes()[..]].concat()
    }

    fn from(k: &'txn [u8], v: &'txn [u8]) -> Self {
        Self { k, v }
        // let time = u64::from_be_bytes(key[8..16].try_into().unwrap());
        // let kind = u64::from_be_bytes(key[0..8].try_into().unwrap());
        // Self {
        //     time,
        //     uid: uid.to_vec(),
        //     kind,
        // }
    }
    fn uid(&self) -> &[u8] {
        self.v
        // &self.uid
    }
}

impl<'txn> TimeKey for Key<'txn> {
    fn time(&self) -> u64 {
        u64::from_be_bytes(self.k[8..16].try_into().unwrap())
        // self.time
    }

    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time()
            .cmp(&other.time())
            .then_with(|| self.uid().cmp(other.uid()))
    }

    fn change_time(&self, key: &[u8], time: u64) -> Vec<u8> {
        [&key[0..8], &time.to_be_bytes()[..]].concat()
        // Self::encode(self.kind, time)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum MyError {
    #[error(transparent)]
    Db(#[from] Error),
    #[error("long query")]
    LongQuery,
}

pub fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
}

#[test]
pub fn test_scanner() -> Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("nokv-test-lmdb-scanner")
        .tempdir()
        .unwrap();
    let db = Db::open(dir.path())?;

    let opts = ffi::MDB_DUPSORT | ffi::MDB_DUPFIXED | ffi::MDB_INTEGERDUP;

    let tree = db.open_tree(Some("t1"), opts)?;
    let mut writer = db.writer()?;

    for i in 1u64..4 {
        writer.put(&tree, Key::encode(1, 10), i.to_be_bytes())?;
    }

    writer.put(&tree, Key::encode(2, 10), 3u64.to_be_bytes())?;

    for i in 4u64..6 {
        writer.put(&tree, Key::encode(2, 30), i.to_be_bytes())?;
    }

    writer.put(&tree, Key::encode(3, 30), 5u64.to_be_bytes())?;

    for i in 6u64..8 {
        writer.put(&tree, Key::encode(3, 20), i.to_be_bytes())?;
    }

    writer.commit()?;

    let reader = db.reader()?;

    // or
    let mut group = Group::new(false, false, true);
    for i in 1u64..4 {
        let prefix = i.to_be_bytes().to_vec();
        let iter = reader.iter_from(&tree, Bound::Included(&prefix), false);
        let scanner = Scanner::<_, MyError>::new(
            iter,
            prefix.clone(),
            prefix.clone(),
            false,
            None,
            None,
            Box::new(|s, (k, v)| {
                Ok(if k.starts_with(&s.prefix) {
                    MatchResult::Found(Key::from(k, v))
                } else {
                    MatchResult::Stop
                })
            }),
        );
        group.add(Box::new(scanner))?;
    }

    let k = group.next().unwrap()?;
    assert_eq!(k.time(), 10);
    assert_eq!(k.uid(), 1u64.to_be_bytes());

    let k = group.next().unwrap()?;
    assert_eq!(k.time(), 10);
    assert_eq!(k.uid(), 2u64.to_be_bytes());

    let k = group.next().unwrap()?;
    assert_eq!(k.time(), 10);
    assert_eq!(k.uid(), 3u64.to_be_bytes());

    let k = group.next().unwrap()?;
    assert_eq!(k.time(), 20);
    assert_eq!(k.uid(), 6u64.to_be_bytes());

    // and
    let mut group = Group::new(false, true, true);
    for i in 1u64..3 {
        let prefix = i.to_be_bytes().to_vec();
        let iter = reader.iter_from(&tree, Bound::Included(&prefix), false);
        let scanner = Scanner::<_, MyError>::new(
            iter,
            prefix.clone(),
            prefix.clone(),
            false,
            None,
            None,
            Box::new(|s, (k, v)| {
                Ok(if k.starts_with(&s.prefix) {
                    MatchResult::Found(Key::from(k, v))
                } else {
                    MatchResult::Stop
                })
            }),
        );
        group.add(Box::new(scanner))?;
    }

    let k = group.next().unwrap()?;
    assert_eq!(k.uid(), 3u64.to_be_bytes());
    assert!(group.next().is_none());

    // watch
    let mut group = Group::new(false, false, true);
    group.watcher(Box::new(|count| {
        if count > 3 {
            Err(MyError::LongQuery)
        } else {
            Ok(())
        }
    }));
    for i in 1u64..4 {
        let prefix = i.to_be_bytes().to_vec();
        let iter = reader.iter_from(&tree, Bound::Included(&prefix), false);
        let scanner = Scanner::<_, MyError>::new(
            iter,
            prefix.clone(),
            prefix.clone(),
            false,
            None,
            None,
            Box::new(|s, (k, v)| {
                Ok(if k.starts_with(&s.prefix) {
                    MatchResult::Found(Key::from(k, v))
                } else {
                    MatchResult::Stop
                })
            }),
        );
        group.add(Box::new(scanner))?;
    }

    let res = group.try_for_each(|k| k.map(|_k| ()));
    assert!(matches!(res, Err(MyError::LongQuery)));

    Ok(())
}
