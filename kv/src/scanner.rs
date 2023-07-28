use crate::{lmdb::Iter, Error};
use std::{
    cmp::Ordering,
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

/// sort key list by time, smaller ones in the back
/// bigger in the back if reverse
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

    fn cmp(&self, k1: &K, k2: &K) -> Ordering {
        if self.reverse {
            k1.cmp(k2)
        } else {
            k2.cmp(k1)
        }
    }

    pub fn add(&mut self, item: I, key: K) {
        // binary
        // TODO: custom search from bigger index because the incoming data is closer to the left
        // let len = self.inner.len();
        // if len > 0 {
        //     // 4 2 1
        //     if self.cmp(&self.inner[len - 1].1, &key).is_le() {
        //         self.inner.push((item, key));
        //         return;
        //     }
        // }
        let insert_at = match self.inner.binary_search_by(|p| self.cmp(&p.1, &key)) {
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

pub trait GroupItem<'txn, K, E>: Iterator<Item = Result<K, E>> {
    /// Set the watcher for watching number of scans, stop scanning
    fn watcher(&mut self, watcher: ScannerWatcherType<E>);
    /// get the current `next` scan times
    fn cur_times(&self) -> u64;
}

/// Query in a group of scanners in a given time sequence.
/// Get the scanners intersection if and.
type SortItemType = usize;
type GroupItemType<'txn, K, E> = Box<dyn GroupItem<'txn, K, E> + 'txn>;

// https://stackoverflow.com/questions/65203307/how-do-i-create-a-trait-object-that-implements-fn-and-can-be-cloned-to-distinct
// https://github.com/dtolnay/dyn-clone
type ScannerWatcherType<E> = Box<dyn ScannerWatcher<E>>;
//FnMut(u64) -> Result<(), E>
pub trait ScannerWatcher<E>: FnMut(u64) -> Result<(), E> {
    fn clone_box(&self) -> Box<dyn ScannerWatcher<E>>;
}

impl<E, F: FnMut(u64) -> Result<(), E> + Clone + 'static> ScannerWatcher<E> for F {
    fn clone_box(&self) -> Box<dyn ScannerWatcher<E>> {
        Box::new(self.clone())
    }
}

impl<E> Clone for Box<dyn ScannerWatcher<E>> {
    fn clone(&self) -> Self {
        (**self).clone_box()
    }
}

pub struct Group<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    onlyone: Option<GroupItemType<'txn, K, E>>,
    items: Vec<GroupItemType<'txn, K, E>>,
    founds: SortedKeyList<SortItemType, K>,
    pub scan_times: u64,
    cur_times: u64,
    and: bool,
    done: bool,
    // one id has more than one key
    dup: bool,
    watcher: Option<ScannerWatcherType<E>>,
}

impl<'txn, K, E> Group<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    pub fn new(reverse: bool, and: bool, dup: bool) -> Self {
        Self {
            onlyone: None,
            items: Vec::new(),
            founds: SortedKeyList::new(reverse),
            scan_times: 0,
            cur_times: 0,
            and,
            done: false,
            dup,
            watcher: None,
        }
    }

    fn watch(&mut self, add_count: u64) -> Result<(), E> {
        self.scan_times += add_count;
        self.cur_times += add_count;
        if let Some(watcher) = &mut self.watcher {
            watcher(self.scan_times)?;
        }
        Ok(())
    }

    pub fn add(&mut self, scanner: GroupItemType<'txn, K, E>) -> Result<(), E> {
        if self.done {
            return Ok(());
        }
        // only one
        if self.items.is_empty() && self.onlyone.is_none() {
            self.onlyone = Some(scanner);
        } else {
            if self.onlyone.is_some() {
                let s = self.onlyone.take().unwrap();
                self.add_to_list(s)?;
            }
            self.add_to_list(scanner)?;
        }
        Ok(())
    }

    fn add_to_list(&mut self, mut scanner: GroupItemType<'txn, K, E>) -> Result<(), E> {
        if self.done {
            return Ok(());
        }

        let key = self.items.len();

        let item = scanner.next();
        self.scan_times += scanner.cur_times();
        self.cur_times += scanner.cur_times();

        // get the first
        if let Some(item) = item {
            self.founds.add(key, item?);
        } else if self.and {
            self.done = true;
            self.founds.clear();
            return Ok(());
        }

        self.items.push(scanner);

        Ok(())
    }

    fn next_and(&mut self) -> Result<Option<K>, E> {
        // check empty in iterator next, so we can use unwrap
        'go: loop {
            let cur = self.founds.pop().unwrap();
            let key = &cur.1;
            // scanners intersection
            let len = self.founds.len();
            for i in (0..len).rev() {
                let item = &self.founds[i];
                if item.1.cmp(key).is_ne() {
                    let scanner = self.items.get_mut(cur.0).unwrap();
                    let item = scanner.next();
                    let count = scanner.cur_times();
                    self.watch(count)?;

                    if let Some(item) = item {
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
            let scanner = self.items.get_mut(cur.0).unwrap();
            let item = scanner.next();
            let count = scanner.cur_times();
            self.watch(count)?;
            if let Some(item) = item {
                self.founds.add(cur.0, item?);
            } else {
                // One scanner is out of data, stop
                self.founds.clear();
            }
            // all eq
            return Ok(Some(cur.1));
        }
    }

    fn next_or(&mut self) -> Result<Option<K>, E> {
        // or
        let cur;
        if self.dup {
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

            cur = curs.pop().unwrap();

            // scan dup next
            for cur in curs {
                let scanner = self.items.get_mut(cur.0).unwrap();
                let item = scanner.next();
                let count = scanner.cur_times();
                self.watch(count)?;
                if let Some(item) = item {
                    self.founds.add(cur.0, item?);
                }
            }
        } else {
            cur = self.founds.pop().unwrap();
        }

        // next
        let scanner = self.items.get_mut(cur.0).unwrap();
        let item = scanner.next();
        let count = scanner.cur_times();
        self.watch(count)?;
        if let Some(item) = item {
            self.founds.add(cur.0, item?);
        }

        Ok(Some(cur.1))
    }
}

impl<'txn, K, E> Iterator for Group<'txn, K, E>
where
    K: TimeKey,
    E: From<Error>,
{
    type Item = Result<K, E>;
    fn next(&mut self) -> Option<Self::Item> {
        self.cur_times = 0;
        if let Some(scanner) = &mut self.onlyone {
            let item = scanner.next();
            let count = scanner.cur_times();
            if let Err(err) = self.watch(count) {
                Some(Err(err))
            } else {
                item
            }
        } else {
            if self.founds.is_empty() || self.done {
                return None;
            }
            if self.and {
                self.next_and().transpose()
            } else {
                self.next_or().transpose()
            }
        }
    }
}

impl<'txn, K: TimeKey, E: From<Error>> GroupItem<'txn, K, E> for Group<'txn, K, E> {
    /// Set the watcher for watching number of scans, stop scanning
    fn watcher(&mut self, watcher: ScannerWatcherType<E>) {
        self.watcher = Some(watcher.clone());
        if let Some(scanner) = &mut self.onlyone {
            scanner.watcher(watcher.clone());
        }
        for item in &mut self.items {
            item.watcher(watcher.clone());
        }
    }

    fn cur_times(&self) -> u64 {
        self.cur_times
    }
}

// pub enum GroupType<'txn, K, E>
// where
//     K: TimeKey,
//     E: From<Error>,
// {
//     One(Scanner<'txn, K, E>),
//     // scanners,
//     // sortedlist,
//     // dup: one id has more than one key
//     Or(
//         Vec<Scanner<'txn, K, E>>,
//         SortedKeyList<ShortItemType, K>,
//         bool,
//     ),
//     And(Vec<Scanner<'txn, K, E>>, SortedKeyList<ShortItemType, K>),
// }

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
    // total scan times
    times: u64,
    // current next scan times
    cur_times: u64,
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
            cur_times: 0,
        }
    }

    fn next_inner(&mut self) -> Result<Option<K>, E> {
        self.cur_times = 0;
        loop {
            self.times += 1;
            self.cur_times += 1;
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

impl<'txn, K: TimeKey, E: From<Error>> GroupItem<'txn, K, E> for Scanner<'txn, K, E> {
    fn watcher(&mut self, _watcher: ScannerWatcherType<E>) {
        // ignore
    }

    fn cur_times(&self) -> u64 {
        self.cur_times
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
