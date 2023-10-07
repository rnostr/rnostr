use nostr_db::{Db, Error, Event, Filter, Stats};
use std::collections::HashMap;
use std::str::FromStr;
use std::thread::sleep;
use std::time::Duration;

type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct MyEvent {
    id: [u8; 32],
    pubkey: [u8; 32],
    created_at: u64,
    kind: u16,
    tags: Vec<Vec<String>>,
    content: String,
    sig: [u8; 64],
}

impl MyEvent {
    pub fn into_and_build_words(self) -> Event {
        let mut e: Event = self.into();
        e.build_note_words();
        e
    }
}

impl Default for MyEvent {
    fn default() -> Self {
        Self {
            id: [0; 32],
            pubkey: [0; 32],
            created_at: 0,
            kind: 0,
            tags: Vec::new(),
            content: String::new(),
            sig: [0; 64],
        }
    }
}

impl From<MyEvent> for Event {
    fn from(value: MyEvent) -> Self {
        Self::new(
            value.id,
            value.pubkey,
            value.created_at,
            value.kind,
            value.tags,
            value.content,
            value.sig,
        )
        .unwrap()
    }
}

pub fn create_db(t: &str) -> Result<Db> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("nostr-db-test-{}", t))
        .tempdir()
        .unwrap();
    Db::open(dir.path())
}

const PER_NUM: u8 = 30;

fn author(index: u8) -> [u8; 32] {
    let mut pubkey = [0; 32];
    pubkey[29] = 1;
    pubkey[30] = 0;
    pubkey[31] = index;
    pubkey
}

fn id(p: u8, index: u8) -> [u8; 32] {
    let mut id = [0; 32];
    id[29] = 1;
    id[30] = p;
    id[31] = index;
    id
}

#[test]
pub fn test_events() -> Result<()> {
    let db = create_db("test_events")?;
    let json = r#"
    [{
        "content": "Good morning everyone 😃",
        "created_at": 1680690006,
        "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
        "kind": 1,
        "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
        "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
        "tags": [["t", "nostr"]],
        "ip": "127.0.0.1"
      }]
    "#;
    let events: Vec<Event> = serde_json::from_str(json).unwrap();
    db.batch_put(&events)?;
    let event = events.get(0).unwrap();
    {
        let reader = db.reader()?;
        let e1: Option<Event> = db.get(&reader, event.id())?;
        assert!(e1.is_some());
        let e1 = e1.unwrap();
        assert_eq!(e1.id(), event.id());
        assert_eq!(&e1.tags(), &event.tags());
    }

    let li = db.batch_get::<Event, _, _>(vec![event.id()])?;
    assert_eq!(li.len(), 1);

    let li = db.batch_get::<String, _, _>(vec![event.id()])?;
    assert_eq!(li.len(), 1);
    assert!(li[0].contains("nostr"));

    // event id
    let li = db.batch_get::<Vec<u8>, _, _>(vec![event.id()])?;
    assert_eq!(li.len(), 1);
    assert_eq!(&li[0], event.id());
    Ok(())
}

#[test]
pub fn test_events_unexpected() -> Result<()> {
    let db = create_db("test_events_unexpected")?;
    let prefix = 0;
    // long tag
    let long = String::from_utf8(vec![b'X'; 600]).unwrap();
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            tags: vec![vec!["r".to_owned(), long.clone()]],
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 2),
            pubkey: author(2),
            kind: 30001,
            tags: vec![vec!["d".to_owned(), long.to_owned()]],
            created_at: 3,
            ..Default::default()
        }
        .into(),
    ];
    db.batch_put(&events)?;
    Ok(())
}

#[test]
pub fn test_events_expiration() -> Result<()> {
    let db = create_db("test_events_expiration")?;
    let prefix = 0;
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            tags: vec![vec!["expiration".to_owned(), "10".to_owned()]],
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 2),
            pubkey: author(1),
            kind: 1000,
            tags: vec![vec!["expiration".to_owned(), "20".to_owned()]],
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 3),
            pubkey: author(1),
            kind: 1000,
            tags: vec![vec!["expiration".to_owned(), "30".to_owned()]],
            ..Default::default()
        }
        .into(),
    ];
    db.batch_put(&events)?;

    {
        let reader = db.reader()?;
        let mut iter = db.iter_expiration::<Event, _>(&reader, Some(20))?;
        let e1 = iter.next().unwrap()?;
        assert_eq!(e1.id(), &id(prefix, 2));
        let e1 = iter.next().unwrap()?;
        assert_eq!(e1.id(), &id(prefix, 1));
        assert!(iter.next().is_none());
    }
    {
        let reader = db.reader()?;
        // del
        let iter = db.iter_expiration::<Event, _>(&reader, Some(20))?;
        let events = iter.map(|e| e.unwrap()).collect::<Vec<_>>();
        db.batch_del(events.iter().map(|e| e.id()))?;
    }
    {
        let reader = db.reader()?;
        let mut iter = db.iter_expiration::<Event, _>(&reader, Some(20))?;
        assert!(iter.next().is_none());
    }
    Ok(())
}

#[test]
pub fn test_events_replace() -> Result<()> {
    let db = create_db("test_events_replace")?;
    let prefix = 0;
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 0,
            created_at: 1,
            ..Default::default()
        }
        .into(),
        // dup
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 0,
            created_at: 1,
            ..Default::default()
        }
        .into(),
        // replace
        MyEvent {
            id: id(prefix, 2),
            pubkey: author(1),
            kind: 0,
            created_at: 2,
            ..Default::default()
        }
        .into(),
        // ignore
        MyEvent {
            id: id(prefix, 3),
            pubkey: author(1),
            kind: 0,
            created_at: 1,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 4),
            pubkey: author(2),
            kind: 30001,
            tags: vec![
                vec!["d".to_owned(), "m".to_owned()],
                // del id 2, correct author
            ],
            created_at: 2,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 5),
            pubkey: author(2),
            kind: 30001,
            tags: vec![vec!["d".to_owned(), "n".to_owned()]],
            created_at: 3,
            ..Default::default()
        }
        .into(),
        // replace
        MyEvent {
            id: id(prefix, 6),
            pubkey: author(2),
            kind: 30001,
            tags: vec![vec!["d".to_owned(), "m".to_owned()]],
            created_at: 3,
            ..Default::default()
        }
        .into(),
    ];

    db.batch_put(&events)?;
    {
        let reader = db.reader()?;
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 1))?.is_none());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 2))?.is_some());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 3))?.is_none());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 4))?.is_none());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 5))?.is_some());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 6))?.is_some());
    }

    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 7),
            pubkey: author(2),
            kind: 30001,
            tags: vec![vec!["d".to_owned(), "n".to_owned()]],
            created_at: 2,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 8),
            pubkey: author(2),
            kind: 30001,
            tags: vec![vec!["d".to_owned(), "m".to_owned()]],
            created_at: 4,
            ..Default::default()
        }
        .into(),
    ];

    let count = db.batch_put(&events)?;
    assert_eq!(count, 2);
    db.batch_put(&events)?;
    {
        let reader = db.reader()?;
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 5))?.is_some());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 6))?.is_none());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 7))?.is_none());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 8))?.is_some());
    }
    Ok(())
}

#[test]
pub fn test_events_del() -> Result<()> {
    let db = create_db("test_events_del")?;
    let prefix = 0;
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            ..Default::default()
        }
        .into(),
        // dup
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            ..Default::default()
        }
        .into(),
        // deleted in the events
        MyEvent {
            id: id(prefix, 2),
            pubkey: author(2),
            kind: 1000,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 3),
            pubkey: author(2),
            kind: 1000,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 4),
            pubkey: author(2),
            kind: 5,
            tags: vec![
                // del id 1, invalid author
                vec!["e".to_owned(), hex::encode(id(prefix, 1))],
                // del id 2, correct author
                vec!["e".to_owned(), hex::encode(id(prefix, 2))],
            ],
            ..Default::default()
        }
        .into(),
    ];
    // del in the events
    let count = db.batch_put(&events)?;
    assert_eq!(count, events.len());
    {
        let reader = db.reader()?;
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 1))?.is_some());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 2))?.is_none());
    }

    let events: Vec<Event> = vec![MyEvent {
        id: id(prefix, 5),
        pubkey: author(2),
        kind: 5,
        tags: vec![
            // del id 1, invalid author
            vec!["e".to_owned(), hex::encode(id(prefix, 1))],
            // del ok
            vec!["e".to_owned(), hex::encode(id(prefix, 3))],
            // del id 4, it's deletion event ignore
            vec!["e".to_owned(), hex::encode(id(prefix, 4))],
        ],
        ..Default::default()
    }
    .into()];
    let count = db.batch_put(&events)?;
    assert_eq!(count, 2);
    {
        let reader = db.reader()?;
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 4))?.is_some());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 5))?.is_some());
        assert!(db.get::<Event, _, _>(&reader, id(prefix, 3))?.is_none());
    }

    Ok(())
}

#[test]
pub fn test_events_dup() -> Result<()> {
    let db = create_db("test_events_dup")?;
    let prefix = 0;
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            ..Default::default()
        }
        .into(),
    ];
    // dup in the events
    let count = db.batch_put(&events)?;
    assert_eq!(count, 1);

    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            ..Default::default()
        }
        .into(),
    ];
    // dup in the db
    let count = db.batch_put(&events)?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
pub fn test_events_delegator() -> Result<()> {
    let db = create_db("test_events_delegator")?;
    let prefix = 0;
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 1000,
            tags: vec![vec!["t".to_owned(), "query tag".to_owned()]],
            created_at: 1,
            ..Default::default()
        }
        .into(),
        // delegation
        MyEvent {
            id: id(prefix, 2),
            pubkey: author(2),
            kind: 1000,
            created_at: 2,
            tags: vec![
                vec!["delegation".to_owned(), hex::encode(author(1))],
                vec!["t".to_owned(), "query tag".to_owned()],
            ],
            ..Default::default()
        }
        .into(),
    ];

    db.batch_put(&events)?;
    let filter = Filter {
        authors: vec![author(1)].into(),
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 2);

    // query by tag
    let filter = Filter {
        authors: vec![author(1)].into(),
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 2);

    // del by delegator
    let events: Vec<Event> = vec![MyEvent {
        id: id(prefix, 4),
        pubkey: author(1),
        kind: 5,
        tags: vec![vec!["e".to_owned(), hex::encode(id(prefix, 2))]],
        created_at: 4,
        ..Default::default()
    }
    .into()];

    db.batch_put(&events)?;
    let filter = Filter {
        authors: vec![author(1)].into(),
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 2);
    assert_eq!(e1.0[0].id(), &id(prefix, 1));
    // del event
    assert_eq!(e1.0[1].id(), &id(prefix, 4));

    Ok(())
}
// fn read(file: &str) -> String {
//     let mut d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
//     d.push(file);
//     std::fs::read_to_string(&d).unwrap()
// }

fn all(db: &Db, filter: &Filter) -> Result<(Vec<Event>, Stats)> {
    let reader = db.reader()?;
    let mut iter = db.iter(&reader, &filter)?;
    let mut events = Vec::new();
    while let Some(e) = iter.next() {
        let e = e.unwrap();
        events.push(e);
    }
    Ok((events, iter.stats()))
}

fn count(db: &Db, filter: &Filter) -> Result<(u64, Stats)> {
    let reader = db.reader()?;
    let iter = db.iter::<String, _>(&reader, filter)?;
    iter.size()
}

#[test]
pub fn test_query_count() -> Result<()> {
    let db = create_db("test_query_count")?;
    // author 250 time
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(30, i),
                pubkey: author(250),
                kind: 1,
                content: "author time".to_owned(),
                created_at: i as u64,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    // prefix break time range
    let filter = Filter {
        desc: true,
        ..Default::default()
    };
    let e1 = count(&db, &filter)?;
    assert_eq!(e1.0, PER_NUM as u64);

    let filter = Filter {
        desc: true,
        limit: Some(10),
        ..Default::default()
    };
    let e1 = count(&db, &filter)?;

    assert!(e1.1.scan_index >= 10);
    assert!(e1.1.get_data == 0);
    assert!(e1.1.get_index == 0);

    assert_eq!(e1.0, 10);
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 10);

    assert!(e1.1.scan_index >= 10);
    assert!(e1.1.get_data == 10);
    assert!(e1.1.get_index == 0);

    Ok(())
}

#[test]
pub fn test_query_authors_by_prefix() -> Result<()> {
    let db = create_db("test_query_authors_by_prefix")?;
    // author 250 time
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(30, i),
                pubkey: author(250),
                kind: 1,
                content: "author time".to_owned(),
                created_at: i as u64,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;
    // author 251 time
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(35, i),
                pubkey: author(251),
                kind: 1,
                content: "author time".to_owned(),
                created_at: i as u64,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    Ok(())
}

#[test]
pub fn test_query_author_kinds() -> Result<()> {
    let db = create_db("test_query_author_kinds")?;
    // author 2 kind
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(15, i),
                pubkey: author(20),
                kind: 1000 + (i / 2) as u16,
                content: "author 2 kind".to_owned(),
                created_at: i as u64 * 1000,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;
    // author 2 kind time
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(16, i),
                pubkey: author(20),
                kind: 1000 + (i / 2) as u16,
                content: "author 2 kind".to_owned(),
                created_at: 100_000 + i as u64 * 1000,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let filter = Filter {
        authors: vec![author(20)].into(),
        kinds: vec![1000, 1001].into(),
        desc: false,
        ..Default::default()
    };

    let e1 = count(&db, &filter)?;
    assert_eq!(e1.0, 8);
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 8);
    assert_eq!(e1.0[0].created_at(), 0);
    for i in 0..7 {
        assert!(e1.0[i].created_at() <= e1.0[i + 1].created_at());
    }
    // rev
    let filter = Filter {
        authors: vec![author(20)].into(),
        kinds: vec![1000, 1001].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 8);
    for i in 0..7 {
        assert!(e1.0[i].created_at() >= e1.0[i + 1].created_at());
    }

    Ok(())
}

#[test]
pub fn test_query_authors() -> Result<()> {
    let db = create_db("test_query_authors")?;

    // author 1
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(0, i),
                pubkey: author(10),
                kind: 1,
                content: "author 1".to_owned(),
                created_at: i as u64 * 1000,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;
    // author 2 tag
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(25, i),
                pubkey: author(20),
                kind: 1 + i as u16,
                content: "author 2 kind".to_owned(),
                created_at: i as u64 * 1000,
                tags: vec![vec!["t".to_owned(), "query tag1".to_owned()]],
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let filter = Filter {
        authors: vec![author(10)].into(),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), PER_NUM as usize);

    let filter = Filter {
        authors: vec![author(20)].into(),
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag1".to_string().into_bytes()].into(),
        )]),
        kinds: vec![1, 2, 3].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);

    Ok(())
}

#[test]
pub fn test_query_created_at() -> Result<()> {
    let db = create_db("test_query_created_at")?;
    // author 2 time
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(20, i),
                pubkey: author(20),
                kind: 1,
                content: "author 2 time".to_owned(),
                created_at: 1_000_000 + i as u64,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let filter = Filter {
        since: Some(2_000_000),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 0);

    let filter = Filter {
        since: Some(1_000_000),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), PER_NUM as usize);

    let filter = Filter {
        since: Some(1_000_000),
        until: Some(1_000_005),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 6);
    assert_eq!(e1.0[0].created_at(), 1_000_000);
    assert_eq!(e1.0[5].created_at(), 1_000_005);

    // rev
    let filter = Filter {
        since: Some(1_000_000),
        until: Some(1_000_005),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 6);
    assert_eq!(e1.0[0].created_at(), 1_000_005);
    assert_eq!(e1.0[5].created_at(), 1_000_000);

    Ok(())
}

#[test]
pub fn test_query_real_time() -> Result<()> {
    let db = create_db("test_query_real_time")?;
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(20, 0),
            pubkey: author(20),
            kind: 1,
            content: "time".to_owned(),
            created_at: 1688479232,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(20, 1),
            pubkey: author(20),
            kind: 1,
            content: "time".to_owned(),
            created_at: 1691570719,
            ..Default::default()
        }
        .into(),
    ];
    db.batch_put(events)?;

    let filter = Filter {
        since: Some(1692290847),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 0);
    Ok(())
}

#[test]
pub fn test_query_tag() -> Result<()> {
    let db = create_db("test_query_tag")?;

    // author 3 tag
    let key = hex::encode(author(1));
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(10, i),
                pubkey: author(30),
                kind: i as u16,
                content: "author 3 tag".to_owned(),
                created_at: i as u64 * 1000,
                tags: vec![
                    vec!["t".to_owned(), "query tag".to_owned()],
                    vec!["e".to_owned(), key.clone()],
                    vec!["p".to_owned(), key.clone()],
                ],
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    // author 2 tag
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(25, i),
                pubkey: author(20),
                kind: i as u16,
                content: "author 2 kind".to_owned(),
                created_at: i as u64 * 1000,
                tags: vec![
                    vec!["t".to_owned(), "query tag1".to_owned()],
                    vec!["k".to_owned(), i.to_string()],
                ],
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let json = format!(r###"{{"#e":["{}"]}}"###, key);
    let filter = Filter::from_str(&json).unwrap();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 30);
    let json = format!(r###"{{"#p":["{}"]}}"###, key);
    let filter = Filter::from_str(&json).unwrap();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 30);

    let filter = Filter {
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 30);

    let filter = Filter {
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 30);

    let filter = Filter {
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        kinds: vec![1, 2, 3].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);

    let filter = Filter {
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag1".to_string().into_bytes()].into(),
        )]),
        kinds: vec![1, 2, 3].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);

    let filter = Filter {
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        kinds: vec![1, 2, 3].into(),
        authors: vec![author(20)].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 0);

    let filter = Filter {
        tags: HashMap::from([
            (
                "t".to_string().into_bytes(),
                vec!["query tag1".to_string().into_bytes()].into(),
            ),
            (
                "k".to_string().into_bytes(),
                vec![1.to_string().into_bytes()].into(),
            ),
        ]),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 1);
    Ok(())
}

#[test]
pub fn test_query_kinds() -> Result<()> {
    let db = create_db("test_query_kinds")?;

    // author 2 kind
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(15, i),
                pubkey: author(20),
                kind: 1000 + (i / 2) as u16,
                content: "author 2 kind".to_owned(),
                created_at: i as u64 * 1000,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;
    // author 2 kind time
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(16, i),
                pubkey: author(20),
                kind: 1000 + (i / 2) as u16,
                content: "author 2 kind".to_owned(),
                created_at: 100_000 + i as u64 * 1000,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let filter = Filter {
        kinds: vec![1001, 1002, 1003].into(),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 12);
    assert_eq!(e1.0[0].kind(), 1001);
    assert_eq!(e1.0[1].kind(), 1001);

    let filter = Filter {
        kinds: vec![1000, 1001, 1002].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 12);
    assert_eq!(e1.0[0].kind(), 1002);
    assert_eq!(e1.0[1].kind(), 1002);

    Ok(())
}

#[test]
pub fn test_query_ids() -> Result<()> {
    let db = create_db("test_query_ids")?;
    let prefix = 0;
    // author 1
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(prefix, i),
                pubkey: author(10),
                kind: 1,
                content: "author 1".to_owned(),
                created_at: i as u64 * 1000,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    // author 3 tag
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(10, i),
                pubkey: author(30),
                kind: i as u16,
                content: "author 3 tag".to_owned(),
                created_at: i as u64 * 1000,
                tags: vec![vec!["t".to_owned(), "query tag".to_owned()]],
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let filter = Filter {
        ids: vec![id(prefix, 0)].into(),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 1);
    assert_eq!(e1.0[0].id(), &id(prefix, 0));

    let filter = Filter {
        ids: vec![id(prefix, 2), id(prefix, 4), id(prefix, 3)].into(),
        desc: false,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);
    assert_eq!(e1.0[0].id(), &id(prefix, 2));
    assert_eq!(e1.0[1].id(), &id(prefix, 3));
    assert_eq!(e1.0[2].id(), &id(prefix, 4));

    // desc
    let filter = Filter {
        ids: vec![id(prefix, 2), id(prefix, 4), id(prefix, 3)].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);
    assert_eq!(e1.0[0].id(), &id(prefix, 4));
    assert_eq!(e1.0[1].id(), &id(prefix, 3));
    assert_eq!(e1.0[2].id(), &id(prefix, 2));

    // desc
    let filter = Filter {
        ids: vec![id(prefix, 2), id(prefix, 4), id(prefix, 3)].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);
    assert_eq!(e1.0[0].id(), &id(prefix, 4));
    assert_eq!(e1.0[1].id(), &id(prefix, 3));
    assert_eq!(e1.0[2].id(), &id(prefix, 2));

    let filter = Filter {
        ids: vec![id(10, 1), id(10, 2), id(10, 3)].into(),
        authors: vec![author(30)].into(),
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        kinds: vec![1, 2, 3, 4].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);

    let filter = Filter {
        ids: vec![id(10, 1), id(10, 2), id(10, 3)].into(),
        authors: vec![author(20)].into(),
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        kinds: vec![1, 2, 3, 4].into(),
        desc: true,
        ..Default::default()
    };
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 0);

    Ok(())
}

#[test]
pub fn test_query_search() -> Result<()> {
    let db = create_db("test_query_search")?;
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(10, i),
                pubkey: author(1),
                kind: 1,
                content: "my note".to_owned(),
                created_at: i as u64 * 1000,
                tags: vec![vec!["t".to_owned(), "query tag".to_owned()]],
                ..Default::default()
            }
            .into_and_build_words()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(20, i),
                pubkey: author(2),
                kind: 1,
                content: "my tag 中文".to_owned(),
                created_at: i as u64 * 1000,
                tags: vec![vec!["t".to_owned(), "query tag".to_owned()]],
                ..Default::default()
            }
            .into_and_build_words()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let filter = Filter {
        search: Some("my".to_string()),
        desc: false,
        ..Default::default()
    };
    // filter.build_words();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 0);

    let mut filter = Filter {
        search: Some("my".to_string()),
        desc: false,
        ..Default::default()
    };
    filter.build_words();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), (PER_NUM * 2) as usize);

    let mut filter = Filter {
        search: Some("my note".to_string()),
        desc: false,
        ..Default::default()
    };
    filter.build_words();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), PER_NUM as usize);

    let mut filter = Filter {
        search: Some("my 中文".to_string()),
        desc: false,
        ..Default::default()
    };
    filter.build_words();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), PER_NUM as usize);

    let mut filter = Filter {
        ids: vec![id(10, 1), id(10, 2), id(10, 3)].into(),
        authors: vec![author(1)].into(),
        tags: HashMap::from([(
            "t".to_string().into_bytes(),
            vec!["query tag".to_string().into_bytes()].into(),
        )]),
        kinds: vec![1, 2, 3, 4].into(),
        search: Some("my note".to_string()),
        desc: false,
        ..Default::default()
    };
    filter.build_words();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), 3);

    assert!(e1.1.scan_index >= PER_NUM as u64);
    assert!(e1.1.get_data == 3);
    assert!(e1.1.get_index > 0);

    // del
    let events = (0..PER_NUM).map(|i| id(10, i)).collect::<Vec<_>>();
    db.batch_del(events)?;
    let mut filter = Filter {
        search: Some("my".to_string()),
        desc: true,
        ..Default::default()
    };
    filter.build_words();
    let e1 = all(&db, &filter)?;
    assert_eq!(e1.0.len(), PER_NUM as usize);

    assert!(e1.1.scan_index >= PER_NUM as u64);
    assert!(e1.1.scan_index < (PER_NUM * 2) as u64);
    assert!(e1.1.get_data == PER_NUM as u64);
    assert!(e1.1.get_index == 0);

    Ok(())
}

#[test]
pub fn test_query_scan_limit_time() -> Result<()> {
    let db = create_db("test_query_scan_limit_time")?;

    // author 1
    let events = (0..PER_NUM)
        .map(|i| {
            MyEvent {
                id: id(0, i),
                pubkey: author(10),
                kind: 1,
                content: "author 1".to_owned(),
                created_at: i as u64 * 1000,
                ..Default::default()
            }
            .into()
        })
        .collect::<Vec<Event>>();
    db.batch_put(events)?;

    let filter = Filter {
        ..Default::default()
    };

    {
        let reader = db.reader()?;
        let mut iter = db.iter::<Event, _>(&reader, &filter)?;
        iter.scan_time(Duration::from_millis(100), 2);
        let res = iter.try_for_each(|k| {
            sleep(Duration::from_millis(50));
            k.map(|_k| ())
        });
        assert!(matches!(res, Err(Error::ScanTimeout)));
    }

    Ok(())
}

#[test]
pub fn test_events_ephemeral() -> Result<()> {
    let db = create_db("test_events_ephemeral")?;
    let prefix = 0;
    let events: Vec<Event> = vec![
        MyEvent {
            id: id(prefix, 1),
            pubkey: author(1),
            kind: 20000,
            created_at: 10,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 2),
            pubkey: author(1),
            kind: 20000,
            created_at: 20,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 3),
            pubkey: author(1),
            kind: 20001,
            created_at: 10,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 4),
            pubkey: author(1),
            kind: 20001,
            created_at: 20,
            ..Default::default()
        }
        .into(),
        MyEvent {
            id: id(prefix, 5),
            pubkey: author(1),
            kind: 30000,
            created_at: 1,
            ..Default::default()
        }
        .into(),
    ];
    db.batch_put(&events)?;

    {
        let reader = db.reader()?;
        let mut iter = db.iter_ephemeral::<Event, _>(&reader, Some(10))?;
        let e1 = iter.next().unwrap()?;
        assert_eq!(e1.id(), &id(prefix, 1));
        let e1 = iter.next().unwrap()?;
        assert_eq!(e1.id(), &id(prefix, 3));
        assert!(iter.next().is_none());
    }
    {
        let reader = db.reader()?;
        let iter = db.iter_ephemeral::<Event, _>(&reader, Some(20))?;
        assert_eq!(iter.count(), 4);
    }
    {
        let reader = db.reader()?;
        let iter = db.iter_ephemeral::<Event, _>(&reader, Some(15))?;
        assert_eq!(iter.count(), 2);
    }
    {
        let reader = db.reader()?;
        // del
        let iter = db.iter_ephemeral::<Event, _>(&reader, Some(10))?;
        let events = iter.map(|e| e.unwrap()).collect::<Vec<_>>();
        db.batch_del(events.iter().map(|e| e.id()))?;
    }
    {
        let reader = db.reader()?;
        let mut iter = db.iter_ephemeral::<Event, _>(&reader, Some(10))?;
        assert!(iter.next().is_none());
    }
    Ok(())
}
