//! Deserialize a JSON string or array of strings into a Vec.
//! The strings separated by whitespace.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::ops::{Deref, DerefMut};
use std::{fmt, marker::PhantomData};

#[derive(Default, Clone, Debug)]
pub struct List(pub Vec<String>);
impl<'de> Deserialize<'de> for List {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        string_or_seq_string(deserializer).map(List)
    }
}

impl Serialize for List {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl Deref for List {
    type Target = Vec<String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for List {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Vec<String>> for List {
    fn from(v: Vec<String>) -> Self {
        Self(v)
    }
}

fn string_or_seq_string<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringOrVec(PhantomData<Vec<String>>);

    impl<'de> de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("string or list of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value.split(' ').map(|s| s.to_owned()).collect())
        }

        fn visit_seq<S>(self, visitor: S) -> Result<Self::Value, S::Error>
        where
            S: de::SeqAccess<'de>,
        {
            Deserialize::deserialize(de::value::SeqAccessDeserializer::new(visitor))
        }
    }

    deserializer.deserialize_any(StringOrVec(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list() -> anyhow::Result<()> {
        let li: List = serde_json::from_str("[\"a\", \"b\"]")?;
        assert_eq!(li[0], "a");
        assert_eq!(li[1], "b");
        let li: List = serde_json::from_str("\"a b\"")?;
        assert_eq!(li[0], "a");
        assert_eq!(li[1], "b");
        let li: List = serde_json::from_str("\"a\"")?;
        assert_eq!(li[0], "a");
        assert_eq!(li.len(), 1);
        Ok(())
    }
}
