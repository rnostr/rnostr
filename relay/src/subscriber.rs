use std::{
    collections::HashMap,
    rc::{Rc, Weak},
};

use crate::{message::*, setting::SettingWrapper};
use actix::prelude::*;
use nostr_db::{EventIndex, Filter};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct Key {
    session_id: usize,
    sub_id: String,
    index: usize,
}

impl Key {
    fn new(session_id: usize, sub_id: String, index: usize) -> Self {
        Self {
            session_id,
            sub_id,
            index,
        }
    }
}

fn concat_tag<K, I>(key: K, val: I) -> Vec<u8>
where
    K: AsRef<[u8]>,
    I: AsRef<[u8]>,
{
    [key.as_ref(), val.as_ref()].concat()
}

// index for fast filter
#[derive(Debug, Default)]
pub struct SubscriberIndex {
    /// map session_id -> subscription_id -> filters
    subscriptions: HashMap<usize, HashMap<String, Vec<Rc<Filter>>>>,
    ids: HashMap<[u8; 32], HashMap<Key, Weak<Filter>>>,
    authors: HashMap<[u8; 32], HashMap<Key, Weak<Filter>>>,
    tags: HashMap<Vec<u8>, HashMap<Key, Weak<Filter>>>,
    kinds: HashMap<u16, HashMap<Key, Weak<Filter>>>,
    others: HashMap<Key, Weak<Filter>>,
}

impl SubscriberIndex {
    fn install_index(&mut self, session_id: usize, sub_id: String, filters: &[Rc<Filter>]) {
        for (index, filter) in filters.iter().enumerate() {
            if !filter.ids.is_empty() {
                for key in filter.ids.iter() {
                    self.ids.entry(*key).or_default().insert(
                        Key::new(session_id, sub_id.clone(), index),
                        Rc::downgrade(filter),
                    );
                }
            } else if !filter.authors.is_empty() {
                for key in filter.authors.iter() {
                    self.authors.entry(*key).or_default().insert(
                        Key::new(session_id, sub_id.clone(), index),
                        Rc::downgrade(filter),
                    );
                }
            } else if !filter.tags.is_empty() {
                for (tag, values) in filter.tags.iter() {
                    for val in values.iter() {
                        self.tags.entry(concat_tag(tag, val)).or_default().insert(
                            Key::new(session_id, sub_id.clone(), index),
                            Rc::downgrade(filter),
                        );
                    }
                }
            } else if !filter.kinds.is_empty() {
                for key in filter.kinds.iter() {
                    self.kinds.entry(*key).or_default().insert(
                        Key::new(session_id, sub_id.clone(), index),
                        Rc::downgrade(filter),
                    );
                }
            } else {
                self.others.insert(
                    Key::new(session_id, sub_id.clone(), index),
                    Rc::downgrade(filter),
                );
            }
        }
    }

    fn uninstall_index(&mut self, session_id: usize, limit_sub_id: Option<&String>) {
        if let Some(subs) = self.subscriptions.get(&session_id) {
            for (sub_id, filters) in subs {
                if let Some(limit_sub_id) = limit_sub_id {
                    if limit_sub_id != sub_id {
                        continue;
                    }
                }
                for (index, filter) in filters.iter().enumerate() {
                    if !filter.ids.is_empty() {
                        for key in filter.ids.iter() {
                            if let Some(map) = self.ids.get_mut(key) {
                                map.remove(&Key::new(session_id, sub_id.clone(), index));
                                if map.is_empty() {
                                    self.ids.remove(key);
                                }
                            }
                        }
                    } else if !filter.authors.is_empty() {
                        for key in filter.authors.iter() {
                            if let Some(map) = self.authors.get_mut(key) {
                                map.remove(&Key::new(session_id, sub_id.clone(), index));
                                if map.is_empty() {
                                    self.authors.remove(key);
                                }
                            }
                        }
                    } else if !filter.tags.is_empty() {
                        for (tag, values) in filter.tags.iter() {
                            for val in values.iter() {
                                let key = concat_tag(tag, val);
                                if let Some(map) = self.tags.get_mut(&key) {
                                    map.remove(&Key::new(session_id, sub_id.clone(), index));
                                    if map.is_empty() {
                                        self.tags.remove(&key);
                                    }
                                }
                            }
                        }
                    } else if !filter.kinds.is_empty() {
                        for key in filter.kinds.iter() {
                            if let Some(map) = self.kinds.get_mut(key) {
                                map.remove(&Key::new(session_id, sub_id.clone(), index));
                                if map.is_empty() {
                                    self.kinds.remove(key);
                                }
                            }
                        }
                    } else {
                        self.others
                            .remove(&Key::new(session_id, sub_id.clone(), index));
                    }
                }
            }
        }
    }

    pub fn add(
        &mut self,
        session_id: usize,
        sub_id: String,
        filters: Vec<Filter>,
        limit: usize,
    ) -> Subscribed {
        // according to NIP-01, <subscription_id> is an arbitrary, non-empty string of max length 64 chars
        if sub_id.is_empty() || sub_id.len() > 64 {
            return Subscribed::InvalidIdLength;
        }

        if let Some(subs) = self.subscriptions.get(&session_id) {
            if subs.len() >= limit {
                return Subscribed::Overlimit;
            }
        }

        let filters = filters.into_iter().map(Rc::new).collect::<Vec<_>>();

        // remove old
        self.uninstall_index(session_id, Some(&sub_id));
        self.install_index(session_id, sub_id.clone(), &filters);

        let map = self.subscriptions.entry(session_id).or_default();

        // NIP01: overwrite the previous subscription
        map.insert(sub_id, filters);
        Subscribed::Ok
    }

    pub fn remove(&mut self, session_id: usize, sub_id: Option<&String>) {
        self.uninstall_index(session_id, sub_id);
        if let Some(sub_id) = sub_id {
            if let Some(map) = self.subscriptions.get_mut(&session_id) {
                map.remove(sub_id);
                if map.is_empty() {
                    self.subscriptions.remove(&session_id);
                }
            }
        } else {
            self.subscriptions.remove(&session_id);
        }
    }

    pub fn lookup(&self, event: &EventIndex, mut f: impl FnMut(&usize, &String)) {
        let mut dup = HashMap::new();

        fn check(
            session_id: usize,
            sub_id: &String,
            filter: &Weak<Filter>,
            event: &EventIndex,
            dup: &mut HashMap<(usize, String), bool>,
            mut f: impl FnMut(&usize, &String),
        ) {
            if let Some(filter) = filter.upgrade() {
                if filter.r#match(event) {
                    let key = (session_id, sub_id.clone());
                    if dup.get(&key).is_none() {
                        f(&session_id, sub_id);
                        dup.insert(key, true);
                    }
                }
            }
        }

        fn scan<T: std::cmp::Eq + std::hash::Hash>(
            map: &HashMap<T, HashMap<Key, Weak<Filter>>>,
            key: &T,
            event: &EventIndex,
            dup: &mut HashMap<(usize, String), bool>,
            mut f: impl FnMut(&usize, &String),
        ) {
            if let Some(map) = map.get(key) {
                for (k, filter) in map {
                    check(k.session_id, &k.sub_id, filter, event, dup, &mut f);
                }
            }
        }

        scan(&self.ids, event.id(), event, &mut dup, &mut f);
        scan(&self.authors, event.pubkey(), event, &mut dup, &mut f);
        scan(&self.kinds, &event.kind(), event, &mut dup, &mut f);
        for (key, val) in event.tags() {
            scan(&self.tags, &concat_tag(key, val), event, &mut dup, &mut f);
        }

        for (k, filter) in &self.others {
            check(k.session_id, &k.sub_id, filter, event, &mut dup, &mut f);
        }
    }

    pub fn lookup1(&self, event: &EventIndex, mut f: impl FnMut(&usize, &String)) {
        for (session_id, subs) in &self.subscriptions {
            for (sub_id, filters) in subs {
                for filter in filters {
                    if filter.r#match(event) {
                        f(session_id, sub_id);
                        break;
                    }
                }
            }
        }
    }
}

pub struct Subscriber {
    pub addr: Recipient<SubscribeResult>,
    /// map session_id -> subscription_id -> filters
    pub subscriptions: HashMap<usize, HashMap<String, Vec<Filter>>>,
    pub index: SubscriberIndex,
    pub setting: SettingWrapper,
}

impl Subscriber {
    pub fn new(addr: Recipient<SubscribeResult>, setting: SettingWrapper) -> Self {
        Self {
            addr,
            subscriptions: HashMap::new(),
            setting,
            index: SubscriberIndex::default(),
        }
    }
}

impl Actor for Subscriber {
    type Context = Context<Self>;
    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.set_mailbox_capacity(10000);
    }
}

impl Handler<Subscribe> for Subscriber {
    type Result = Subscribed;
    fn handle(&mut self, msg: Subscribe, _: &mut Self::Context) -> Subscribed {
        self.index.add(
            msg.id,
            msg.subscription.id,
            msg.subscription.filters,
            self.setting.read().limitation.max_subscriptions,
        )
    }
}

impl Handler<Unsubscribe> for Subscriber {
    type Result = ();
    fn handle(&mut self, msg: Unsubscribe, _: &mut Self::Context) {
        self.index.remove(msg.id, msg.sub_id.as_ref());
    }
}

impl Handler<Dispatch> for Subscriber {
    type Result = ();
    fn handle(&mut self, msg: Dispatch, _: &mut Self::Context) {
        let event = &msg.event;
        let index = event.index();
        let event_str = event.to_string();
        self.index.lookup(index, |session_id, sub_id| {
            self.addr.do_send(SubscribeResult {
                id: *session_id,
                msg: OutgoingMessage::event(sub_id, &event_str),
                sub_id: sub_id.clone(),
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::Setting;

    use super::*;
    use actix_rt::time::sleep;
    use anyhow::Result;
    use nostr_db::{Event, Filter};
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::{str::FromStr, time::Duration};

    #[derive(Default)]
    struct Receiver(Arc<RwLock<Vec<SubscribeResult>>>);
    impl Actor for Receiver {
        type Context = Context<Self>;
    }

    impl Handler<SubscribeResult> for Receiver {
        type Result = ();
        fn handle(&mut self, msg: SubscribeResult, _ctx: &mut Self::Context) {
            self.0.write().push(msg);
        }
    }

    #[actix_rt::test]
    async fn subscribe() -> Result<()> {
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
        let event = Event::from_str(note)?;

        let receiver = Receiver::default();
        let messages = receiver.0.clone();
        let receiver = receiver.start();
        let addr = receiver.recipient();

        let subscriber = Subscriber::new(addr.clone(), Setting::default().into()).start();

        subscriber
            .send(Dispatch {
                id: 0,
                event: event.clone(),
            })
            .await?;

        sleep(Duration::from_millis(100)).await;
        {
            let r = messages.read();
            assert_eq!(r.len(), 0);
            drop(r);
        }

        let res = subscriber
            .send(Subscribe {
                id: 0,
                subscription: Subscription {
                    id: 0.to_string(),
                    filters: vec![Filter {
                        ..Default::default()
                    }],
                },
            })
            .await?;
        assert_eq!(res, Subscribed::Ok);

        // overwrite
        let res = subscriber
            .send(Subscribe {
                id: 0,
                subscription: Subscription {
                    id: 0.to_string(),
                    filters: vec![Filter {
                        ..Default::default()
                    }],
                },
            })
            .await?;
        assert_eq!(res, Subscribed::Ok);

        let res = subscriber
            .send(Subscribe {
                id: 0,
                subscription: Subscription {
                    id: 1.to_string(),
                    filters: vec![Filter {
                        kinds: vec![1000].into(),
                        ..Default::default()
                    }],
                },
            })
            .await?;
        assert_eq!(res, Subscribed::Ok);

        let res = subscriber
            .send(Subscribe {
                id: 0,
                subscription: Subscription {
                    id: "".to_string(),
                    filters: vec![Filter {
                        kinds: vec![1000].into(),
                        ..Default::default()
                    }],
                },
            })
            .await?;
        assert_eq!(res, Subscribed::InvalidIdLength);

        let res = subscriber
            .send(Subscribe {
                id: 0,
                subscription: Subscription {
                    id: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdefA"
                        .to_string(),
                    filters: vec![Filter {
                        kinds: vec![1000].into(),
                        ..Default::default()
                    }],
                },
            })
            .await?;
        assert_eq!(res, Subscribed::InvalidIdLength);

        subscriber
            .send(Dispatch {
                id: 0,
                event: event.clone(),
            })
            .await?;

        sleep(Duration::from_millis(100)).await;
        let r = messages.read();
        assert_eq!(r.len(), 1);
        drop(r);

        Ok(())
    }

    fn lookup(index: &SubscriberIndex, event: &str) -> Result<Vec<(usize, String)>> {
        let event = Event::from_str(event)?;
        let mut result = vec![];
        let mut result1 = vec![];
        index.lookup(event.index(), |session_id, sub_id| {
            result.push((*session_id, sub_id.clone()));
        });
        index.lookup1(event.index(), |session_id, sub_id| {
            result1.push((*session_id, sub_id.clone()));
        });
        result.sort();
        result1.sort();
        assert_eq!(result, result1);
        Ok(result)
    }

    // fn gen_id(p: u8, index: u8) -> [u8; 32] {
    //     let mut id = [0; 32];
    //     id[29] = 1;
    //     id[30] = p;
    //     id[31] = index;
    //     id
    // }

    #[test]
    fn index() -> Result<()> {
        let mut index = SubscriberIndex::default();
        // all
        index.add(
            1,
            "all".to_owned(),
            vec![Filter::from_str("{}")?, Filter::from_str("{}")?],
            5,
        );

        index.add(
            1,
            "id".to_owned(),
            vec![
                Filter::from_str(
                    r###"
         {
            "ids": ["0000000000000000000000000000000000000000000000000000000000000000", 
                    "0000000000000000000000000000000000000000000000000000000000000001"]
          }
        "###,
                )?,
                Filter::from_str(
                    r###"
         {
            "ids": ["0000000000000000000000000000000000000000000000000000000000000000"]
          }
        "###,
                )?,
            ],
            5,
        );
        index.add(
            2,
            "author".to_owned(),
            vec![
                Filter::from_str(
                    r###"
         {
            "authors": ["0000000000000000000000000000000000000000000000000000000000000000", 
                    "0000000000000000000000000000000000000000000000000000000000000001"]
          }
        "###,
                )?,
                Filter::from_str(
                    r###"
         {
            "authors": ["0000000000000000000000000000000000000000000000000000000000000000"]
          }
        "###,
                )?,
            ],
            5,
        );
        index.add(
            3,
            "kind".to_owned(),
            vec![
                Filter::from_str(
                    r###"
         {
            "kinds": [0, 1]
          }
        "###,
                )?,
                Filter::from_str(
                    r###"
         {
            "kinds": [0]
          }
        "###,
                )?,
            ],
            5,
        );
        index.add(
            4,
            "tag1".to_owned(),
            vec![
                Filter::from_str(
                    r###"
         {
            "#p": ["0000000000000000000000000000000000000000000000000000000000000000", 
                    "0000000000000000000000000000000000000000000000000000000000000001"]
          }
        "###,
                )?,
                Filter::from_str(
                    r###"
         {
            "#p": ["0000000000000000000000000000000000000000000000000000000000000000"]
          }
        "###,
                )?,
            ],
            5,
        );
        index.add(
            4,
            "tag2".to_owned(),
            vec![Filter::from_str(
                r###"
         {
            "#p": ["0000000000000000000000000000000000000000000000000000000000000000", 
                    "0000000000000000000000000000000000000000000000000000000000000001"],
                    "#t": ["test"]
          }
        "###,
            )?],
            5,
        );
        // override
        let ok = index.add(
            4,
            "tag2".to_owned(),
            vec![Filter::from_str(
                r###"
         {
            "#p": ["0000000000000000000000000000000000000000000000000000000000000000", 
                    "0000000000000000000000000000000000000000000000000000000000000001"],
                    "#t": ["test"]
          }
        "###,
            )?],
            5,
        );
        assert_eq!(ok, Subscribed::Ok);
        assert_eq!(index.others.len(), 2);
        assert_eq!(index.ids.len(), 2);
        assert_eq!(index.authors.len(), 2);
        assert_eq!(index.kinds.len(), 2);
        assert_eq!(index.tags.len(), 3);

        let res = lookup(
            &index,
            r###"
        {
           "id": "0000000000000000000000000000000000000000000000000000000000000000",
           "pubkey": "0000000000000000000000000000000000000000000000000000000000000001",
           "kind": 1,
           "tags": [],
           "content": "",
           "created_at": 0,
           "sig": "633db60e2e7082c13a47a6b19d663d45b2a2ebdeaf0b4c35ef83be2738030c54fc7fd56d139652937cdca875ee61b51904a1d0d0588a6acd6168d7be2909d693"
         }
       "###,
        )?;
        assert_eq!(res.len(), 4);
        let res = lookup(
            &index,
            r###"
        {
           "id": "0000000000000000000000000000000000000000000000000000000000000002",
           "pubkey": "0000000000000000000000000000000000000000000000000000000000000001",
           "kind": 1,
           "tags": [],
           "content": "",
           "created_at": 0,
           "sig": "633db60e2e7082c13a47a6b19d663d45b2a2ebdeaf0b4c35ef83be2738030c54fc7fd56d139652937cdca875ee61b51904a1d0d0588a6acd6168d7be2909d693"
         }
       "###,
        )?;
        assert_eq!(res.len(), 3);

        let res = lookup(
            &index,
            r###"
        {
           "id": "0000000000000000000000000000000000000000000000000000000000000008",
           "pubkey": "0000000000000000000000000000000000000000000000000000000000000008",
           "kind": 10,
           "tags": [["p", "0000000000000000000000000000000000000000000000000000000000000000"]],
           "content": "",
           "created_at": 0,
           "sig": "633db60e2e7082c13a47a6b19d663d45b2a2ebdeaf0b4c35ef83be2738030c54fc7fd56d139652937cdca875ee61b51904a1d0d0588a6acd6168d7be2909d693"
         }
       "###,
        )?;
        assert_eq!(res.len(), 2);

        let res = lookup(
            &index,
            r###"
        {
           "id": "0000000000000000000000000000000000000000000000000000000000000008",
           "pubkey": "0000000000000000000000000000000000000000000000000000000000000008",
           "kind": 10,
           "tags": [["p", "0000000000000000000000000000000000000000000000000000000000000000"], ["t", "test"]],
           "content": "",
           "created_at": 0,
           "sig": "633db60e2e7082c13a47a6b19d663d45b2a2ebdeaf0b4c35ef83be2738030c54fc7fd56d139652937cdca875ee61b51904a1d0d0588a6acd6168d7be2909d693"
         }
       "###,
        )?;
        assert_eq!(res.len(), 3);

        index.remove(1, Some(&"all".to_owned()));
        index.remove(1, Some(&"id".to_owned()));
        index.remove(2, Some(&"author".to_owned()));
        index.remove(3, Some(&"kind".to_owned()));
        index.remove(4, Some(&"tag1".to_owned()));
        index.remove(4, Some(&"tag2".to_owned()));

        assert_eq!(index.subscriptions.len(), 0);
        assert_eq!(index.others.len(), 0);
        assert_eq!(index.ids.len(), 0);
        assert_eq!(index.authors.len(), 0);
        assert_eq!(index.kinds.len(), 0);
        assert_eq!(index.tags.len(), 0);
        Ok(())
    }
}
