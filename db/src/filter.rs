use crate::{error::Error, ArchivedEventIndex, EventIndex};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashMap, ops::Deref, str::FromStr};

/// The tag list contains unduplicated and sorted items
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct TagList(Vec<Vec<u8>>);

impl From<Vec<Vec<u8>>> for TagList {
    fn from(mut value: Vec<Vec<u8>>) -> Self {
        value.sort();
        value.dedup();
        Self(value)
    }
}

impl TagList {
    pub fn contains<I: AsRef<[u8]>>(&self, item: I) -> bool {
        self.binary_search_by(|p| p.deref().cmp(item.as_ref()))
            .is_ok()
        // self.0.deref().binary_search(item.as_ref()).is_ok()
    }
}

impl Deref for TagList {
    type Target = Vec<Vec<u8>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Events filter
///
/// [NIP-01](https://nips.be/1)

// TODO: hashset uniq, (default limit), limit length, limit item length, empty string, invald hex prefix, validate length
#[derive(PartialEq, Eq, Debug, Clone, Default, Deserialize)]
#[serde(try_from = "_Filter")]
pub struct Filter {
    /// a list of event ids or prefixes
    pub ids: Option<Vec<String>>,

    /// a list of pubkeys or prefixes, the pubkey of an event must be one of these
    pub authors: Option<Vec<String>>,

    /// a list of a kind numbers
    pub kinds: Option<Vec<u64>>,

    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<u64>,

    /// Keyword search  [NIP-50](https://nips.be/50) , [keywords renamed to search](https://github.com/nostr-protocol/nips/commit/6708a73bbcd141094c75f739c8b31446620b30e1)
    pub search: Option<String>,

    /// tags starts with "#", key tag length 1
    ///
    pub tags: HashMap<Vec<u8>, TagList>,

    /// Query by time descending order
    pub desc: bool,

    #[serde(skip)]
    pub words: Option<Vec<Vec<u8>>>,
}

impl FromStr for Filter {
    type Err = serde_json::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

#[derive(Deserialize)]
struct _Filter {
    pub ids: Option<Vec<String>>,
    pub authors: Option<Vec<String>>,
    pub kinds: Option<Vec<u64>>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<u64>,
    pub keywords: Option<Vec<String>>,
    pub search: Option<String>,
    #[serde(flatten)]
    pub tags: HashMap<String, Value>,
}

impl TryFrom<_Filter> for Filter {
    type Error = Error;
    fn try_from(filter: _Filter) -> Result<Self, Self::Error> {
        // deserialize search option, convert keywords array to string
        let mut search = filter.search;
        if search.is_none() && filter.keywords.is_some() {
            search = Some(filter.keywords.unwrap().join(" "));
        }

        // only use valid tag, has prefix "#", string item, not empty
        let mut tags = HashMap::new();
        for item in filter.tags {
            let key = item.0;
            if let Some(key) = key.strip_prefix('#') {
                let key = key.as_bytes();
                // only index for key len 1
                if key.len() == 1 {
                    let val = Vec::<String>::deserialize(&item.1)?;
                    let mut list = vec![];
                    for s in val {
                        if key == b"e" || key == b"p" {
                            let h = hex::decode(&s)?;
                            if h.len() != 32 {
                                // ignore
                                return Err(Error::Invalid("invalid e or p tag value".to_string()));
                            } else {
                                list.push(h);
                            }
                        } else {
                            list.push(s.into_bytes());
                            // if s.len() < 255 {
                            // } else {
                            //     return Err(Error::Invald("invalid value length".to_string()));
                            // }
                        }
                    }
                    if !list.is_empty() {
                        tags.insert(key.to_vec(), list.into());
                    }
                }
            }
        }

        let ids = clean_empty(filter.ids);
        let authors = clean_empty(filter.authors);
        // let empty = "".to_string();
        // if let Some(a) = ids.as_ref() {
        //     if a.contains(&empty) {
        //         return Err(serde::de::Error::invalid_type(
        //             serde::de::Unexpected::Other("prefix matches must not be empty strings"),
        //             &"a json object",
        //         ));
        //     }
        // }

        let f = Filter {
            ids,
            authors,
            kinds: clean_empty(filter.kinds),
            since: filter.since,
            until: filter.until,
            limit: filter.limit,
            search,
            tags,
            desc: filter.limit.is_some(),
            words: None,
        };

        Ok(f)
    }
}

fn clean_empty<T>(list: Option<Vec<T>>) -> Option<Vec<T>> {
    list.filter(|li| !li.is_empty())
}

impl Filter {
    #[cfg(feature = "search")]
    /// build keywords for search ability
    pub fn build_words(&mut self) {
        if let Some(search) = &self.search {
            let words = crate::segment(search);
            if !words.is_empty() {
                self.words = Some(words);
            }
        }
    }

    pub fn default_limit(&mut self, limit: u64) {
        if self.limit.is_none() {
            self.limit = Some(limit);
        }
    }

    pub fn set_tags(&mut self, tags: HashMap<String, Vec<String>>) {
        let mut t = HashMap::new();
        for item in tags {
            let key = item.0.into_bytes();
            // only index for key len 1
            if key.len() == 1 {
                let val = item
                    .1
                    .into_iter()
                    .map(|s| s.into_bytes())
                    // only index tag value length < 255
                    .filter(|s| s.len() < 255)
                    .collect::<Vec<_>>();
                if !key.is_empty() && !val.is_empty() {
                    t.insert(key, val.into());
                }
            }
        }
        self.tags = t;
    }

    pub fn match_id<K: AsRef<[u8]>>(ids: Option<&Vec<String>>, id: K) -> bool {
        ids.map_or(true, |ids| match_prefix(ids, id.as_ref()))
    }

    pub fn match_author<P: AsRef<[u8]>, D: AsRef<[u8]>>(
        authors: Option<&Vec<String>>,
        pubkey: P,
        delegator: Option<D>,
    ) -> bool {
        authors.map_or(true, |ids| {
            if match_prefix(ids, pubkey.as_ref()) {
                true
            } else if let Some(d) = delegator {
                match_prefix(ids, d.as_ref())
            } else {
                false
            }
        })
    }

    pub fn match_kind(kinds: Option<&Vec<u64>>, kind: u64) -> bool {
        kinds.map_or(true, |ks| ks.contains(&kind))
    }

    pub fn match_tag<V: AsRef<[u8]>, I: AsRef<[(V, V)]>>(
        tags: &HashMap<Vec<u8>, TagList>,
        event_tags: I,
    ) -> bool {
        // empty tags
        if tags.is_empty() {
            return true;
        }

        // event has not tag
        if event_tags.as_ref().is_empty() {
            return false;
        }

        // all tag must match
        for tag in tags.iter() {
            if !Self::tag_contains(&event_tags, tag.0, tag.1) {
                return false;
            }
        }
        true
    }

    fn tag_contains<V: AsRef<[u8]>, I: AsRef<[(V, V)]>>(
        tags: I,
        name: &[u8],
        list: &TagList,
    ) -> bool {
        let tags = tags.as_ref();
        if tags.is_empty() {
            return false;
        }
        for tag in tags {
            if tag.0.as_ref() == name && list.contains(tag.1.as_ref()) {
                return true;
            }
        }
        false
    }

    pub fn r#match(&self, event: &EventIndex) -> bool {
        self.match_except_tag(event) && Self::match_tag(&self.tags, event.tags())
    }

    pub fn match_except_tag(&self, event: &EventIndex) -> bool {
        Self::match_id(self.ids.as_ref(), event.id())
            && self.since.map_or(true, |t| event.created_at() >= t)
            && self.until.map_or(true, |t| event.created_at() <= t)
            && Self::match_kind(self.kinds.as_ref(), event.kind())
            && Self::match_author(self.authors.as_ref(), event.pubkey(), event.delegator())
    }

    pub fn match_archived(&self, event: &ArchivedEventIndex) -> bool {
        self.match_archived_except_tag(event) && Self::match_tag(&self.tags, event.tags())
    }

    pub fn match_archived_except_tag(&self, event: &ArchivedEventIndex) -> bool {
        Self::match_id(self.ids.as_ref(), event.id())
            && self.since.map_or(true, |t| event.created_at() >= t)
            && self.until.map_or(true, |t| event.created_at() <= t)
            && Self::match_kind(self.kinds.as_ref(), event.kind())
            && Self::match_author(self.authors.as_ref(), event.pubkey(), event.delegator())
    }
}

fn match_prefix(prefixes: &[String], target: &[u8]) -> bool {
    if prefixes.is_empty() {
        return true;
    }
    let target = hex::encode(target);
    for prefix in prefixes {
        if target.starts_with(prefix) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr};

    use super::Filter;
    use crate::{filter::TagList, ArchivedEventIndex, Event, EventIndex};
    use anyhow::Result;

    #[test]
    fn deser_filter() -> Result<()> {
        // empty
        let note = "{}";
        let filter: Filter = serde_json::from_str(note)?;
        assert!(filter.tags.is_empty());
        assert!(filter.ids.is_none());

        // valid
        let note = r###"
        {
            "ids": ["ab", "cd", "12"],
            "authors": ["ab", "cd", "12"],
            "kinds": [1, 2],
            "until": 5,
            "since": 3,
            "limit": 6,
            "#d": ["ab", "cd", "12"], 
            "#f": ["ab", "cd", "12", "ab"],
            "#b": [],
            "search": "abc",
            "invalid": ["ab", "cd", "12"],
            "_invalid": 123
          }
        "###;
        let mut filter: Filter = serde_json::from_str(note)?;
        let li = vec!["ab".to_string(), "cd".to_string(), "12".to_string()];
        let tags: TagList = li
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect::<Vec<_>>()
            .into();
        assert_eq!(&filter.ids.as_ref(), &Some(&li));
        assert_eq!(&filter.authors.as_ref(), &Some(&li));
        assert_eq!(&filter.kinds, &Some(vec![1, 2]));
        assert_eq!(filter.until, Some(5));
        assert_eq!(filter.since, Some(3));
        assert_eq!(filter.limit, Some(6));
        assert_eq!(filter.search, Some("abc".to_string()));

        // tag
        assert_eq!(
            &filter.tags.get(&"d".to_string().into_bytes()),
            &Some(&tags)
        );
        // dup
        assert_eq!(
            &filter.tags.get(&"f".to_string().into_bytes()),
            &Some(&tags)
        );
        assert!(filter
            .tags
            .get(&"invalid".to_string().into_bytes())
            .is_none());
        assert!(filter
            .tags
            .get(&"_invalid".to_string().into_bytes())
            .is_none());
        assert!(filter.tags.get(&"b".to_string().into_bytes()).is_none());
        // set tag
        filter.set_tags(HashMap::from([
            (
                "t".to_string(),
                vec![
                    "ab".to_string(),
                    "ab".to_string(),
                    "cd".to_string(),
                    "12".to_string(),
                ],
            ),
            (
                "g".to_string(),
                vec!["ab".to_string(), "cd".to_string(), "12".to_string()],
            ),
        ]));
        assert_eq!(
            &filter.tags.get(&"t".to_string().into_bytes()),
            &Some(&tags)
        );
        assert_eq!(
            &filter.tags.get(&"g".to_string().into_bytes()),
            &Some(&tags)
        );
        assert!(filter.tags.get(&"d".to_string().into_bytes()).is_none());

        // search
        let note = r###"
        {
            "keywords": ["abc", "def"]
          }
        "###;
        let filter: Filter = serde_json::from_str(note)?;
        assert_eq!(filter.search, Some("abc def".to_string()));

        let note = r###"
        {
            "keywords": ["abc", "def"],
            "search": "t"
          }
        "###;
        let filter: Filter = serde_json::from_str(note)?;
        assert_eq!(filter.search, Some("t".to_string()));

        // invalid
        let note = r###"
        {
            "#g": ["ab", "cd", 12]
          }
        "###;
        let filter: Result<Filter, _> = serde_json::from_str(note);
        assert!(filter.is_err());

        let note = r###"
        {
            "#e": ["ab"],
            "#p": ["ab"]
          }
        "###;
        let filter = Filter::from_str(note);
        assert!(filter.is_err());

        let note = r###"
        {
            "#e": ["0000000000000000000000000000000000000000000000000000000000000000"],
            "#p": ["0000000000000000000000000000000000000000000000000000000000000000"]
          }
        "###;
        let filter = Filter::from_str(note)?;
        assert!(filter
            .tags
            .get(&b"e".to_vec())
            .unwrap()
            .contains(vec![0u8; 32]));
        let filter = Filter::from_str(note)?;
        assert!(filter
            .tags
            .get(&b"p".to_vec())
            .unwrap()
            .contains(vec![0u8; 32]));
        Ok(())
    }

    fn check_match(
        s: &str,
        matched: bool,
        event: &Event,
        archived: &ArchivedEventIndex,
    ) -> Result<()> {
        let filter: Filter = serde_json::from_str(s)?;
        if matched {
            assert!(filter.r#match(event.index()));
            assert!(filter.match_archived(archived));
        } else {
            assert!(!filter.r#match(event.index()));
            assert!(!filter.match_archived(archived));
        }
        Ok(())
    }

    #[test]
    fn match_event() -> Result<()> {
        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"], ["t", "db"], ["subject", "db"]]
          }
        "#;
        let event: Event = serde_json::from_str(note)?;
        let bytes = event.index().to_bytes()?;
        let archived = EventIndex::from_zeroes(&bytes)?;

        check_match(
            r###"
        {
        }
        "###,
            true,
            &event,
            archived,
        )?;

        check_match(
            r###"
        {
            "ids": ["33", "other"],
            "authors": ["7a", "other"],
            "kind": [1, 2],
            "#t": ["nostr", "other"],
            "#subject": ["db", "other"],
            "since": 1680690000,
            "util": 2680690000
        }
        "###,
            true,
            &event,
            archived,
        )?;

        check_match(
            r###"
        {
            "#t": ["other"]
        }
        "###,
            false,
            &event,
            archived,
        )?;

        check_match(
            r###"
        {
            "#t": ["nostr"],
            "#r": ["nostr"]
        }
        "###,
            false,
            &event,
            archived,
        )?;

        check_match(
            r###"
        {
            "ids": ["33"]
        }
        "###,
            true,
            &event,
            archived,
        )?;

        check_match(
            r###"
        {
            "ids": ["ab"]
        }
        "###,
            false,
            &event,
            archived,
        )?;

        Ok(())
    }

    #[test]
    fn tag_contains() -> Result<()> {
        let note = r#"
        {
            "content": "Good morning everyone 😃",
            "created_at": 1680690006,
            "id": "332747c0fab8a1a92def4b0937e177be6df4382ce6dd7724f86dc4710b7d4d7d",
            "kind": 1,
            "pubkey": "7abf57d516b1ff7308ca3bd5650ea6a4674d469c7c5057b1d005fb13d218bfef",
            "sig": "ef4ff4f69ac387239eb1401fb07d7a44a5d5d57127e0dc3466a0403cf7d5486b668608ebfcbe9ff1f8d3b5d710545999fe08ee767284ec0b474e4cf92537678f",
            "tags": [["t", "nostr"], ["t", "db"], ["r", "db"]]
          }
        "#;
        let event: Event = serde_json::from_str(note)?;
        assert!(Filter::tag_contains(
            event.index().tags(),
            &"t".to_string().into_bytes(),
            &vec!["nostr".to_string().into_bytes()].into()
        ));
        assert!(Filter::tag_contains(
            event.index().tags(),
            &"t".to_string().into_bytes(),
            &vec![
                "nostr".to_string().into_bytes(),
                "other".to_string().into_bytes()
            ]
            .into()
        ));

        assert!(!Filter::tag_contains(
            event.index().tags(),
            &"t".to_string().into_bytes(),
            &vec![
                "nostr1".to_string().into_bytes(),
                "other".to_string().into_bytes()
            ]
            .into()
        ));
        Ok(())
    }
}
