use governor::{
    clock::DefaultClock, state::keyed::DashMapStateStore, Quota, RateLimiter as GovernorRateLimiter,
};
use metrics::{describe_counter, increment_counter};
use nostr_relay::db::Event;
use nostr_relay::{
    duration::NonZeroDuration,
    message::{ClientMessage, IncomingMessage, OutgoingMessage},
    setting::SettingWrapper,
    Extension, ExtensionMessageResult, Session,
};
use parking_lot::RwLock;
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use std::{
    fmt,
    marker::PhantomData,
    num::NonZeroU32,
    ops::Deref,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Deserialize, Debug)]
pub struct EventQuota {
    /// used by metrics
    #[serde(default)]
    pub name: String,
    /// description will notice the user when rate limiter exceeded
    #[serde(default)]
    pub description: String,
    pub period: NonZeroDuration,
    pub limit: NonZeroU32,
    /// only limit for kinds
    /// support kind list: [1, 2, 3]
    /// kind ranges included(start) to excluded(end): [[0, 10000], [30000, 40000]]
    /// mixed: [1, 2, [30000, 40000]]
    pub kinds: Option<Vec<Range>>,
    pub ip_whitelist: Option<Vec<String>>,
}

/// a simple range included(start)..excluded(end)
#[derive(Debug, PartialEq, Eq)]
pub struct Range(pub u64, pub u64);

impl Range {
    pub fn contains(&self, value: u64) -> bool {
        value >= self.0 && value < self.1
    }
}

struct RangeVisitor(PhantomData<()>);
impl<'de> Visitor<'de> for RangeVisitor {
    type Value = Range;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("sequence")
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Range(v, v + 1))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let lower: u64 = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let upper: u64 = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        Ok(Range(lower, upper))
    }
}

impl<'de> Deserialize<'de> for Range {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(RangeVisitor(PhantomData))
    }
}

impl EventQuota {
    pub fn hit(&self, event: &Event, ip: &String) -> bool {
        if let Some(list) = &self.ip_whitelist {
            if list.contains(ip) {
                return false;
            }
        }
        if let Some(list) = &self.kinds {
            for range in list {
                if range.contains(event.kind() as u64) {
                    return true;
                }
            }
            // out of range
            return false;
        }
        // has not condition
        true
    }
}

pub trait Quotable {
    fn limit(&self) -> NonZeroU32;
    fn period(&self) -> NonZeroDuration;
    fn quota(&self) -> Quota {
        Quota::with_period(Duration::from_nanos(
            (self.period().as_nanos() / self.limit().get() as u128) as u64,
        ))
        .unwrap()
        .allow_burst(self.limit())
    }
}

impl Quotable for EventQuota {
    fn limit(&self) -> NonZeroU32 {
        self.limit
    }
    fn period(&self) -> NonZeroDuration {
        self.period
    }
}

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct RatelimiterSetting {
    pub enabled: bool,
    /// write event rate limiter: ["EVENT"]
    pub event: Vec<EventQuota>,
    /// interval at second for clearing invalid data to free up memory.
    /// default 60 non zero
    pub clear_interval: NonZeroDuration,
}

impl Default for RatelimiterSetting {
    fn default() -> Self {
        Self {
            enabled: Default::default(),
            event: Default::default(),
            clear_interval: Duration::from_secs(60).try_into().unwrap(),
        }
    }
}

type Limiters = Vec<GovernorRateLimiter<String, DashMapStateStore<String>, DefaultClock>>;

#[derive(Debug)]
pub struct Ratelimiter {
    pub setting: RatelimiterSetting,
    pub event_limiters: Limiters,
    pub clear_time: Arc<RwLock<Instant>>,
}

impl Default for Ratelimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Ratelimiter {
    pub fn new() -> Self {
        describe_counter!(
            "nostr_relay_rate_limiter_exceeded",
            "The total count of rate limiter exceeded messages"
        );
        Self {
            setting: Default::default(),
            event_limiters: Default::default(),
            clear_time: Arc::new(RwLock::new(Instant::now())),
        }
    }

    pub fn clear(&self) {
        if &self.clear_time.read().elapsed() > self.setting.clear_interval.deref() {
            {
                let mut w = self.clear_time.write();
                *w = Instant::now();
            }
            for limiter in &self.event_limiters {
                limiter.retain_recent();
            }
        }
    }
}

impl Extension for Ratelimiter {
    fn name(&self) -> &'static str {
        "rate_limiter"
    }

    fn setting(&mut self, setting: &SettingWrapper) {
        self.setting = setting.read().parse_extension(self.name());
        self.event_limiters = self
            .setting
            .event
            .iter()
            .map(|q| GovernorRateLimiter::dashmap(q.quota()))
            .collect::<Vec<_>>();
    }

    fn message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        _ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        // enabled
        if self.setting.enabled {
            self.clear();
            let ip = session.ip();
            if let IncomingMessage::Event(event) = &msg.msg {
                // check event limiter
                for (index, limiter) in self.event_limiters.iter().enumerate() {
                    let q = &self.setting.event[index];
                    if q.hit(event, ip) && limiter.check_key(ip).is_err() {
                        increment_counter!("nostr_relay_rate_limiter_exceeded", "command" => "EVENT", "name" => q.name.clone());
                        return OutgoingMessage::ok(
                            &event.id_str(),
                            false,
                            &format!("rate-limited: {}", q.description),
                        )
                        .into();
                    }
                }
            }
        }
        ExtensionMessageResult::Continue(msg)
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, str::FromStr, time::Duration};

    use super::*;
    use crate::create_test_app;
    use actix_rt::time::sleep;
    use actix_web::web;
    use actix_web_actors::ws;
    use anyhow::Result;
    use futures_util::{SinkExt as _, StreamExt as _};
    use nostr_relay::db::{
        now,
        secp256k1::{rand::thread_rng, KeyPair},
    };
    use nostr_relay::{create_web_app, Setting};

    fn parse_text<T: serde::de::DeserializeOwned>(frame: &ws::Frame) -> Result<T> {
        if let ws::Frame::Text(text) = &frame {
            let data: T = serde_json::from_slice(text)?;
            Ok(data)
        } else {
            Err(nostr_relay::Error::Message("invalid frame type".to_string()).into())
        }
    }

    #[test]
    fn quota() -> Result<()> {
        let period = 1;
        let limit: u32 = 10;
        let q = Quota::with_period(Duration::from_nanos(
            (Duration::from_secs(period).as_nanos() / limit as u128) as u64,
        ))
        .unwrap()
        .allow_burst(NonZeroU32::new(10).unwrap());
        assert_eq!(q, Quota::per_second(NonZeroU32::new(10).unwrap()));
        Ok(())
    }

    #[test]
    fn range() -> Result<()> {
        let json = "[1, 2, [30000, 40000]]";
        let ranges: Vec<Range> = serde_json::from_str(json)?;
        assert_eq!(ranges.len(), 3);
        assert!(ranges[0].contains(1));
        assert!(!ranges[0].contains(0));
        assert!(!ranges[0].contains(2));
        assert!(ranges[2].contains(30000));
        assert!(ranges[2].contains(30001));
        assert!(ranges[2].contains(39999));
        assert!(!ranges[2].contains(40000));
        assert_eq!(ranges, vec![Range(1, 2), Range(2, 3), Range(30000, 40000)]);

        let json = "[\"1\", 2, [30000, 40000]]";
        let ranges = serde_json::from_str::<Vec<Range>>(json);
        assert!(ranges.is_err());
        Ok(())
    }

    #[test]
    fn hit() -> Result<()> {
        let event = Event::from_str(
            r#"{"kind":1, "id": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "pubkey": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "created_at": 1, "sig": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
        )?;
        let ip = "127.0.0.1".to_owned();

        let q = EventQuota {
            name: "test".to_owned(),
            description: Default::default(),
            period: Duration::from_secs(1).try_into().unwrap(),
            limit: NonZeroU32::new(1).unwrap(),
            kinds: None,
            ip_whitelist: None,
        };
        assert!(q.hit(&event, &ip));

        let q = EventQuota {
            name: "test".to_owned(),
            description: Default::default(),
            period: Duration::from_secs(1).try_into().unwrap(),
            limit: NonZeroU32::new(1).unwrap(),
            kinds: Some(vec![Range(1, 100), Range(200, 300)]),
            ip_whitelist: Some(vec![ip.clone()]),
        };
        // ip whitelist
        assert!(!q.hit(&event, &ip));
        // kinds
        assert!(q.hit(&event, &"127".to_owned()));
        let event = Event::from_str(
            r#"{"kind":101, "id": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "pubkey": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "created_at": 1, "sig": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
        )?;
        assert!(!q.hit(&event, &"127".to_owned()));
        Ok(())
    }

    #[actix_rt::test]
    async fn check() -> Result<()> {
        let setting: SettingWrapper = Setting::default().into();
        {
            let mut w = setting.write();
            w.extra = serde_json::from_str(
                r#"{
                "rate_limiter": {
                    "enabled": true,
                    "event": [{
                        "period": 1,
                        "limit": 3
                    }]
                }
            }"#,
            )?;
        }
        let mut limiter = Ratelimiter::new();
        limiter.setting(&setting);
        assert!(limiter.setting.enabled);
        assert_eq!(limiter.event_limiters.len(), 1);
        let lim = &limiter.event_limiters[0];
        let ip = "127.0.0.1".to_owned();
        assert!(lim.check_key(&ip).is_ok());
        assert!(lim.check_key(&ip).is_ok());
        assert!(lim.check_key(&ip).is_ok());
        assert!(lim.check_key(&ip).is_err());
        sleep(Duration::from_millis(100)).await;
        assert!(lim.check_key(&ip).is_err());
        sleep(Duration::from_millis(1100)).await;
        assert!(lim.check_key(&ip).is_ok());
        Ok(())
    }

    #[actix_rt::test]
    async fn message() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = KeyPair::new_global(&mut rng);

        let app = create_test_app("rate_limiter")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(
                r#"{
                "rate_limiter": {
                    "enabled": true,
                    "event": [{
                        "period": 1,
                        "limit": 2,
                        "kinds": [1, 2, [100, 200]]
                    }]
                }
            }"#,
            )?;
        }

        let app = app.add_extension(Ratelimiter::new());
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        for _ in 0..2 {
            let event = Event::create(&key_pair, now(), 1, vec![], "test".to_owned())?;
            framed
                .send(ws::Message::Text(
                    format!(r#"["EVENT", {}]"#, event.to_string()).into(),
                ))
                .await?;
            let notice: (String, String, bool, String) =
                parse_text(&framed.next().await.unwrap()?)?;
            assert!(notice.2);
        }

        // rate limit
        let event = Event::create(&key_pair, now(), 1, vec![], "test".to_owned())?;
        framed
            .send(ws::Message::Text(
                format!(r#"["EVENT", {}]"#, event.to_string()).into(),
            ))
            .await?;
        let notice: (String, String, bool, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert!(!notice.2);
        assert!(notice.3.contains("rate-limited"));

        // not hit kinds
        for _ in 0..5 {
            let event = Event::create(&key_pair, now(), 3, vec![], "test".to_owned())?;
            framed
                .send(ws::Message::Text(
                    format!(r#"["EVENT", {}]"#, event.to_string()).into(),
                ))
                .await?;
            let notice: (String, String, bool, String) =
                parse_text(&framed.next().await.unwrap()?)?;
            assert!(notice.2);
        }

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        Ok(())
    }
}
