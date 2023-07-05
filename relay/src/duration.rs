//! flexible duration deserializer
//! deserialize format:
//! map: {"secs": 1, "nanos": 1}
//! list: [1, 1]
//! u64 as seconds: 1
//! str by [`duration_str`]: 3m+1s
//!
use duration_str::parse;
use serde::{
    de::{Error, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::{fmt, ops::Deref, time::Duration};

/// Deserialize a `Duration`
pub fn deserialize<'a, D>(d: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'a>,
{
    d.deserialize_any(DurationVisitor)
}

/// Serialize a `Duration`
pub fn serialize<S>(d: &Duration, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    d.serialize(s)
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(into = "Duration")]
pub struct NonZeroDuration(Duration);

impl NonZeroDuration {
    pub fn new(value: Duration) -> Option<Self> {
        if value.is_zero() {
            None
        } else {
            Some(Self(value))
        }
    }
}

// impl Into<Duration> for NonZeroDuration {
//     fn into(self) -> Duration {
//         self.0
//     }
// }

impl From<NonZeroDuration> for Duration {
    fn from(val: NonZeroDuration) -> Self {
        val.0
    }
}

impl TryFrom<Duration> for NonZeroDuration {
    type Error = &'static str;
    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err("duration can't be zero")
        } else {
            Ok(Self(value))
        }
    }
}

impl Deref for NonZeroDuration {
    type Target = Duration;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for NonZeroDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_any(DurationVisitor)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "lowercase")]
enum Field {
    Secs,
    Nanos,
}

fn check_overflow<E>(secs: u64, nanos: u32) -> Result<(), E>
where
    E: Error,
{
    static NANOS_PER_SEC: u32 = 1_000_000_000;
    match secs.checked_add((nanos / NANOS_PER_SEC) as u64) {
        Some(_) => Ok(()),
        None => Err(E::custom("overflow deserializing SystemTime epoch offset")),
    }
}

// source: https://github.com/serde-rs/serde/blob/20a48c9580445b82e570c237159e4bce8b95831b/serde/src/de/impls.rs#L2037
struct DurationVisitor;

impl<'de> Visitor<'de> for DurationVisitor {
    type Value = Duration;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("struct Duration")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let secs: u64 = match seq.next_element()? {
            Some(value) => value,
            None => {
                return Err(Error::invalid_length(0, &self));
            }
        };
        let nanos: u32 = match seq.next_element()? {
            Some(value) => value,
            None => {
                return Err(Error::invalid_length(1, &self));
            }
        };
        check_overflow(secs, nanos)?;
        Ok(Duration::new(secs, nanos))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut secs: Option<u64> = None;
        let mut nanos: Option<u32> = None;
        while let Some(key) = map.next_key()? {
            match key {
                Field::Secs => {
                    if secs.is_some() {
                        return Err(<A::Error as Error>::duplicate_field("secs"));
                    }
                    secs = Some(map.next_value()?);
                }
                Field::Nanos => {
                    if nanos.is_some() {
                        return Err(<A::Error as Error>::duplicate_field("nanos"));
                    }
                    nanos = Some(map.next_value()?);
                }
            }
        }
        let secs = match secs {
            Some(secs) => secs,
            None => return Err(<A::Error as Error>::missing_field("secs")),
        };
        let nanos = match nanos {
            Some(nanos) => nanos,
            None => return Err(<A::Error as Error>::missing_field("nanos")),
        };
        check_overflow(secs, nanos)?;
        Ok(Duration::new(secs, nanos))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        check_overflow(v, 0)?;
        Ok(Duration::from_secs(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        parse(v).map_err(Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[derive(Deserialize, Serialize)]
    struct Test {
        #[serde(with = "super")]
        time: Duration,
    }
    #[test]
    fn der() -> Result<()> {
        let t = serde_json::from_str::<Test>(r#"{"time": 1}"#)?;
        assert_eq!(t.time, Duration::from_secs(1));

        let t = serde_json::from_str::<Test>(r#"{"time": "1m"}"#)?;
        assert_eq!(t.time, Duration::from_secs(60));

        let t = serde_json::from_str::<Test>(r#"{"time": [1, 1]}"#)?;
        assert_eq!(t.time, Duration::new(1, 1));

        let t = serde_json::from_str::<Test>(r#"{"time": {"secs": 1, "nanos": 1}}"#)?;
        assert_eq!(t.time, Duration::new(1, 1));

        let t = serde_json::from_str::<Test>(r#"{"time": "1m"}"#)?;
        let json = serde_json::to_string(&t)?;
        let t = serde_json::from_str::<Test>(&json)?;
        assert_eq!(t.time, Duration::from_secs(60));

        let t = serde_json::from_str::<Test>(r#"{"time": 0}"#)?;
        assert_eq!(t.time, Duration::from_secs(0));
        Ok(())
    }

    #[derive(Deserialize, Serialize)]
    struct TestNonZero {
        time: NonZeroDuration,
    }
    #[test]
    fn non_zero() -> Result<()> {
        let t = serde_json::from_str::<TestNonZero>(r#"{"time": 1}"#)?;
        assert_eq!(t.time, Duration::from_secs(1).try_into().unwrap());

        let t = serde_json::from_str::<TestNonZero>(r#"{"time": "1m"}"#)?;
        assert_eq!(t.time, Duration::from_secs(60).try_into().unwrap());

        let t = serde_json::from_str::<TestNonZero>(r#"{"time": [1, 1]}"#)?;
        assert_eq!(t.time, Duration::new(1, 1).try_into().unwrap());

        let t = serde_json::from_str::<TestNonZero>(r#"{"time": {"secs": 1, "nanos": 1}}"#)?;
        assert_eq!(t.time, Duration::new(1, 1).try_into().unwrap());

        let t = serde_json::from_str::<TestNonZero>(r#"{"time": "1m"}"#)?;
        let json = serde_json::to_string(&t)?;
        let t = serde_json::from_str::<TestNonZero>(&json)?;
        assert_eq!(t.time, Duration::from_secs(60).try_into().unwrap());

        let t = serde_json::from_str::<TestNonZero>(r#"{"time": 0}"#);
        assert!(t.is_err());
        Ok(())
    }
}
