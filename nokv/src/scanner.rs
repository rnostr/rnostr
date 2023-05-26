use crate::{lmdb::Iter, Error};
use std::{
    cmp::Ordering,
    collections::HashMap,
    ops::{Bound, Deref, DerefMut},
};

/// The time base index key
pub trait TimeKey {
    fn time(&self) -> u64;

    // fn uid(&self) -> &[u8];

    fn cmp(&self, other: &Self) -> Ordering {
        self.time().cmp(&other.time())
        // .then_with(|| self.uid().cmp(other.uid()))
    }

    /// change the key time for scan next
    fn change_time(&self, key: &[u8], time: u64) -> Vec<u8>;
}

/// sort key list by time
#[derive(Default, Debug)]
pub struct SortedKeyList<I, K> {
    inner: Vec<(I, K)>,
    reverse: bool,
}

impl<I, K: TimeKey> SortedKeyList<I, K> {
    pub fn new(reverse: bool) -> Self {
        Self {
            inner: Vec::new(),
            reverse,
        }
    }

    pub fn add(&mut self, item: I, key: K) {
        // binary search from middle
        //TODO: we can custom search from bigger index because the incoming data is closer to the left
        let insert_at = match self.inner.binary_search_by(|p| {
            if self.reverse {
                p.1.cmp(&key)
            } else {
                key.cmp(&p.1)
            }
        }) {
            Ok(insert_at) | Err(insert_at) => insert_at,
        };
        self.inner.insert(insert_at, (item, key));
    }
}

impl<I, K> Deref for SortedKeyList<I, K> {
    type Target = Vec<(I, K)>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<I, K> DerefMut for SortedKeyList<I, K> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

type GroupItem<K, E> = Result<K, E>;
/// Query in a group of scanners in a given time sequence.
/// Get the scanners intersection if and.
type ShortItemType = u32;
pub struct Group<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    pub scanners: HashMap<ShortItemType, Scanner<'txn, K, E>>,
    founds: SortedKeyList<ShortItemType, K>,
    pub scan_index: u64,
    and: bool,
    done: bool,
}

impl<'txn, K, E> Group<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    pub fn new(reverse: bool, and: bool) -> Self {
        Group {
            scanners: HashMap::new(),
            founds: SortedKeyList::new(reverse),
            scan_index: 0,
            and,
            done: false,
        }
    }

    pub fn add(&mut self, key: ShortItemType, scanner: Scanner<'txn, K, E>) -> Result<(), E> {
        if self.scanners.contains_key(&key) {
            return Ok(());
        }

        if self.done {
            // empty down
            return Ok(());
        }

        self.scanners.insert(key.clone(), scanner);
        let scanner = self.scanners.get_mut(&key).unwrap();
        let start = scanner.times;
        // get the first
        if let Some(item) = scanner.next() {
            self.founds.add(key.clone(), item?);
        } else if self.and {
            self.done = true;
            self.founds.clear();
            return Ok(());
        }

        self.scan_index += scanner.times - start;
        Ok(())
    }

    fn next_inner(&mut self) -> Result<Option<K>, E> {
        // check empty in iterator next, so we can use unwrap
        if self.and {
            'go: loop {
                let cur = self.founds.pop().unwrap();
                let key = &cur.1;
                // scanners intersection
                let len = self.founds.len();
                for i in (0..len).into_iter().rev() {
                    let item = &self.founds[i];
                    if item.1.cmp(key).is_ne() {
                        let scanner = self.scanners.get_mut(&cur.0).unwrap();
                        self.scan_index += 1;
                        if let Some(item) = scanner.next() {
                            self.founds.add(cur.0, item?);
                            continue 'go;
                        } else {
                            // One scanner is out of data, stop
                            self.founds.clear();
                            return Ok(None);
                        }
                    }
                }

                // scan next
                let scanner = self.scanners.get_mut(&cur.0).unwrap();
                self.scan_index += 1;
                if let Some(item) = scanner.next() {
                    self.founds.add(cur.0, item?);
                } else {
                    // One scanner is out of data, stop
                    self.founds.clear();
                }
                // all eq
                return Ok(Some(cur.1));
            }
        } else {
            // or
            let mut curs = vec![self.founds.pop().unwrap()];

            // dedup
            while self.founds.len() > 0 {
                let item = &self.founds[self.founds.len() - 1];
                if item.1.cmp(&curs[0].1).is_eq() {
                    curs.push(self.founds.pop().unwrap());
                } else {
                    break;
                }
            }

            let cur = curs.pop().unwrap();

            // scan dup next
            for cur in curs {
                let scanner = self.scanners.get_mut(&cur.0).unwrap();
                self.scan_index += 1;
                if let Some(item) = scanner.next() {
                    self.founds.add(cur.0, item?);
                }
            }

            // next
            let scanner = self.scanners.get_mut(&cur.0).unwrap();
            self.scan_index += 1;
            if let Some(item) = scanner.next() {
                self.founds.add(cur.0, item?);
            }

            Ok(Some(cur.1))
        }
    }
}

impl<'txn, K, E> Iterator for Group<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    type Item = GroupItem<K, E>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.founds.is_empty() || self.done {
            return None;
        }
        self.next_inner().transpose()
    }
}

pub struct OneGroup<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    pub scanner: Scanner<'txn, K, E>,
    pub scan_index: u64,
}

impl<'txn, K, E> OneGroup<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    pub fn new(scanner: Scanner<'txn, K, E>) -> Self {
        OneGroup {
            scanner,
            scan_index: 0,
        }
    }
}

impl<'txn, K, E> Iterator for OneGroup<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    type Item = Result<K, E>;
    fn next(&mut self) -> Option<Self::Item> {
        self.scan_index += 1;
        self.scanner.next()
    }
}

type ScannerMatcher<'txn, K, E> =
    Box<dyn Fn(&Scanner<K, E>, (&'txn [u8], &'txn [u8])) -> Result<MatchResult<K>, E>>;

pub enum MatchResult<K> {
    Continue,
    Found(K),
    Stop,
}

/// time base scanner
pub struct Scanner<'txn, K, E>
where
    E: From<Error>,
{
    pub inner: Iter<'txn>,
    // search key bytes
    pub prefix: Vec<u8>,
    // search key
    pub key: Vec<u8>,
    // range start from
    // start: Vec<u8>,
    matcher: ScannerMatcher<'txn, K, E>,
    reverse: bool,
    since: Option<u64>,
    until: Option<u64>,
    // scan times
    times: u64,
}

impl<'txn, K, E> Scanner<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    pub fn new(
        iter: Iter<'txn>,
        key: Vec<u8>,
        prefix: Vec<u8>,
        reverse: bool,
        since: Option<u64>,
        until: Option<u64>,
        matcher: ScannerMatcher<'txn, K, E>,
    ) -> Self {
        Self {
            matcher,
            inner: iter,
            key,
            prefix,
            reverse,
            since,
            until,
            times: 0,
        }
    }

    fn next_inner(&mut self) -> Result<Option<K>, E> {
        loop {
            self.times += 1;
            if let Some(item) = self.inner.next() {
                let item = item?;
                let item_key = item.0;
                match (self.matcher)(self, item)? {
                    MatchResult::Continue => {
                        continue;
                    }

                    MatchResult::Stop => {
                        return Ok(None);
                    }

                    MatchResult::Found(key) => {
                        // check time
                        if self.reverse {
                            if let Some(util) = self.until {
                                if key.time() > util {
                                    // go to the range start match time
                                    self.inner.seek(
                                        Bound::Included(key.change_time(item_key, util)),
                                        true,
                                    );
                                    continue;
                                }
                            }

                            if let Some(since) = self.since {
                                // go to the next range match prefix
                                if key.time() < since {
                                    self.inner
                                        .seek(Bound::Excluded(key.change_time(item_key, 0)), true);
                                    continue;
                                }
                            }
                        } else {
                            if let Some(since) = self.since {
                                if key.time() < since {
                                    // go to the range start match time
                                    self.inner.seek(
                                        Bound::Included(key.change_time(item_key, since)),
                                        false,
                                    );
                                    continue;
                                }
                            }
                            if let Some(util) = self.until {
                                if key.time() > util {
                                    // go to the next range match prefix
                                    self.inner.seek(
                                        Bound::Excluded(key.change_time(item_key, u64::MAX)),
                                        false,
                                    );
                                    continue;
                                }
                            }
                        }
                        return Ok(Some(key));
                    }
                }
            } else {
                return Ok(None);
            }
        }
    }
}

impl<'txn, K, E> Iterator for Scanner<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    type Item = Result<K, E>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_inner().transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn sorted_key_list() -> Result<()> {
        struct Key {
            time: u64,
        }

        impl Key {
            fn new(time: u64) -> Self {
                Self { time }
            }
        }

        impl TimeKey for Key {
            fn time(&self) -> u64 {
                self.time
            }

            fn change_time(&self, _key: &[u8], _time: u64) -> Vec<u8> {
                vec![]
            }
        }

        // reverse
        let mut sl = SortedKeyList::new(true);
        sl.add(vec![1], Key::new(1));
        sl.add(vec![10], Key::new(10));
        sl.add(vec![5], Key::new(5));
        sl.add(vec![6], Key::new(6));

        assert_eq!(sl.len(), 4);
        assert_eq!(sl.pop().map(|a| a.0), Some(vec![10]));
        assert_eq!(sl.pop().map(|a| a.0), Some(vec![6]));
        assert_eq!(sl.len(), 2);

        let mut sl = SortedKeyList::new(false);
        sl.add(vec![1], Key::new(1));
        sl.add(vec![10], Key::new(10));
        sl.add(vec![5], Key::new(5));
        sl.add(vec![6], Key::new(6));

        assert_eq!(sl.len(), 4);
        assert_eq!(sl.pop().map(|a| a.0), Some(vec![1]));
        assert_eq!(sl.pop().map(|a| a.0), Some(vec![5]));
        assert_eq!(sl.len(), 2);

        Ok(())
    }
}
