use std::collections::HashMap;

use crate::{message::*, setting::SettingWrapper};
use actix::prelude::*;
use nostr_db::{Event, Filter};

// TODO: use btree index for fast filter
pub struct Subscriber {
    pub addr: Recipient<SubscribeResult>,
    pub events: Vec<(usize, Event)>,
    /// map session_id -> subscription_id -> filters
    pub subscriptions: HashMap<usize, HashMap<String, Vec<Filter>>>,
    pub setting: SettingWrapper,
}

impl Subscriber {
    pub fn new(addr: Recipient<SubscribeResult>, setting: SettingWrapper) -> Self {
        Self {
            addr,
            events: Vec::new(),
            subscriptions: HashMap::new(),
            setting,
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
        let map = self.subscriptions.entry(msg.id).or_default();
        if map.len() >= self.setting.read().limitation.max_subscriptions {
            Subscribed::Overlimit
        } else {
            let Subscription { id, filters } = msg.subscription;
            // according to NIP-01, <subscription_id> is an arbitrary, non-empty string of max length 64 chars
            if id.is_empty() || id.len() > 64 {
                Subscribed::InvalidIdLength
            } else {
                // NIP01: overwrite the previous subscription
                map.insert(id, filters);
                Subscribed::Ok
            }
        }
    }
}

impl Handler<Unsubscribe> for Subscriber {
    type Result = ();
    fn handle(&mut self, msg: Unsubscribe, _: &mut Self::Context) {
        if let Some(sub_id) = msg.sub_id {
            if let Some(map) = self.subscriptions.get_mut(&msg.id) {
                map.remove(&sub_id);
            }
        } else {
            self.subscriptions.remove(&msg.id);
        }
    }
}

impl Handler<Dispatch> for Subscriber {
    type Result = ();
    fn handle(&mut self, msg: Dispatch, _: &mut Self::Context) {
        let event = &msg.event;
        let index = event.index();
        let event_str = event.to_string();
        for (session_id, subs) in &self.subscriptions {
            for (sub_id, filters) in subs {
                for filter in filters {
                    if filter.r#match(index) {
                        self.addr.do_send(SubscribeResult {
                            id: *session_id,
                            msg: OutgoingMessage::event(sub_id, &event_str),
                            sub_id: sub_id.clone(),
                        });
                    }
                }
            }
        }
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
}
