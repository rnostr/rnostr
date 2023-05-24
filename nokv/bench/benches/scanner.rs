#![allow(dead_code, unused)]

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use nokv::{lmdb::Transaction, scanner::*, Error};
use nokv_bench::*;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Key {
    uid: Vec<u8>,
    kind: u64,
    time: u64,
}

impl Key {
    fn encode(kind: u64, time: u64) -> Vec<u8> {
        [&kind.to_be_bytes()[..], &time.to_be_bytes()[..]].concat()
    }

    fn from(key: &[u8], val: &[u8]) -> Self {
        let time = u64::from_be_bytes(key[8..16].try_into().unwrap());
        let kind = u64::from_be_bytes(key[0..8].try_into().unwrap());
        Self {
            time,
            uid: val.to_vec(),
            kind,
        }
    }
}

impl TimeKey for Key {
    fn time(&self) -> u64 {
        self.time
    }

    fn uid(&self) -> &[u8] {
        &self.uid
    }

    fn change_time(&self, time: u64) -> Vec<u8> {
        Self::encode(self.kind, time)
    }
}

#[derive(Debug)]
struct ZeroKey<'txn> {
    k: &'txn [u8],
    v: &'txn [u8],
}

impl<'txn> ZeroKey<'txn> {
    fn encode(kind: u64, time: u64) -> Vec<u8> {
        [&kind.to_be_bytes()[..], &time.to_be_bytes()[..]].concat()
    }

    fn from(k: &'txn [u8], v: &'txn [u8]) -> Self {
        Self { k, v }
    }
}

impl<'txn> TimeKey for ZeroKey<'txn> {
    fn time(&self) -> u64 {
        u64::from_be_bytes(self.k[8..16].try_into().unwrap())
    }

    fn uid(&self) -> &[u8] {
        self.v
    }

    fn change_time(&self, time: u64) -> Vec<u8> {
        [&self.k[0..8], &time.to_be_bytes()[..]].concat()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum MyError {
    #[error(transparent)]
    Db(#[from] Error),
}

fn bench_scanner1(c: &mut Criterion) {
    bench_scanner(c, 1_000_000, 10_000);
}

fn bench_scanner(c: &mut Criterion, init_len: usize, chunk_size: usize) {
    let num_str = fmt_num(init_len as f64);

    println!("Generate initial data {}", num_str);
    let now = Instant::now();
    let initial = gen_pairs(16, 8, init_len);
    println!("Generated in {:?}", now.elapsed());
    let initial_chunks = chunk_vec(&initial, chunk_size);

    let mut group = c.benchmark_group(format!("scanner-{}-{}", num_str, chunk_size));
    group.measurement_time(Duration::from_secs(3));
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.throughput(Throughput::Elements(init_len as u64));
    let dir = tempfile::Builder::new()
        .prefix("nokv-bench-scanner")
        .tempdir()
        .unwrap();
    let db = nokv::lmdb::Db::open_with(
        dir.path(),
        Some(30),
        Some(1_000),
        Some(1_000_000_000_000),
        0,
    )
    .unwrap();
    let tree = db.open_tree(Some("t1"), 0).unwrap();

    {
        println!("lmdb: Put initial data batch {}", chunk_size);
        let now = Instant::now();
        for chunk in initial_chunks.iter() {
            let mut writer = db.writer().unwrap();
            for (k, v) in chunk {
                writer.put(&tree, k, v).unwrap();
            }
            writer.commit().unwrap();
        }
        println!(
            "put in {:?} {:?}",
            now.elapsed(),
            fmt_per_sec(init_len, &now.elapsed())
        );
    }

    {
        let reader = db.reader().unwrap();
        group.bench_function("count", |b| {
            b.iter(|| {
                let mut iter = reader.iter(&tree);
                black_box(&iter);
                let mut _total = 0;
                while let Some(kv) = iter.next() {
                    let kv = kv.unwrap();
                    // black_box(kv);
                    black_box(ZeroKey::from(kv.0, kv.1));
                    // black_box(Key::from(kv.0, kv.1));
                    _total += 1;
                }
            })
        });
    }
    {
        let reader = db.reader().unwrap();

        group.bench_function("scanner-count", |b| {
            b.iter(|| {
                let iter = reader.iter(&tree);
                let mut group = Group::new(false, false);
                let prefix = vec![];
                let scanner = Scanner::<_, MyError>::new(
                    iter,
                    prefix.clone(),
                    prefix.clone(),
                    false,
                    None,
                    None,
                    Box::new(|s, (k, v)| {
                        Ok(MatchResult::Found(ZeroKey::from(k, v)))
                        // Ok(MatchResult::Found(Key::from(k, v)))
                        // Ok(if k.starts_with(&s.prefix) {
                        //     MatchResult::Found(Key::from(k, v))
                        // } else {
                        //     MatchResult::Stop
                        // })
                    }),
                );
                group.add(prefix.clone(), scanner).unwrap();
                let mut _total = 0;
                while let Some(kv) = group.next() {
                    let kv = kv.unwrap();
                    black_box(kv);
                    _total += 1;
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scanner1);
criterion_main!(benches);
