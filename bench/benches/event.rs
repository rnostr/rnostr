use charabia::{Segment, Tokenize};
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use nostr_db::{Event, EventIndex};
use std::{hash::Hasher, str::FromStr, time::Duration};
use twox_hash::XxHash32;

fn bench_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("event");
    group.measurement_time(Duration::from_secs(1));
    group.sample_size(50);
    group.warm_up_time(Duration::from_millis(100));
    group.throughput(Throughput::Elements(1));

    let json = r###"
    {"content":"Just a resource.中文 I own it, I’ve skimmed it. I’ve read it. I think it’s complete. But I have yet to apply it. No livestock on my property yet. \n\nhttps://a.co/d/fBD7pnc","created_at":1682257408,"id":"d877f51f90134aa0ee5572b393a90126e45f00ddc72242b0f9b47e90f864748c","kind":1,"pubkey":"0cf08d280aa5fcfaf340c269abcf66357526fdc90b94b3e9ff6d347a41f090b7","sig":"7d62ec09612b3e303eb8d105a5c99b2a9df6f5497b14465c235b58db2b0db8d834ee320c6d9ede8722773cddfea926a7fa108b1c829ce2208c773ba8aa44d396","tags":[["e","180f289555764f435ab5529f384fb13a79fc8df737c1b661dbaa966195636ff0"],["p","fc87ad313d6dc741dbed5a89720a7e20000b672dba0a901d9620da4c202242dd"]]}
    "###;
    let event = Event::from_str(json).unwrap();

    group.bench_function("content token", |b| {
        b.iter(|| {
            let s: &str = event.content().as_ref();
            let tokens = s.tokenize();
            for t in tokens {
                black_box(t.lemma());
            }
        })
    });
    group.bench_function("content segment", |b| {
        b.iter(|| {
            let s: &str = event.content().as_ref();
            let tokens = s.segment();
            for t in tokens {
                black_box(t.lemma());
            }
        })
    });
    group.bench_function("content segment with hash", |b| {
        b.iter(|| {
            let s: &str = event.content().as_ref();
            let tokens = s.segment();
            for t in tokens {
                let mut hasher = XxHash32::with_seed(0);
                hasher.write(t.lemma().as_bytes());
                black_box(hasher.finish() as u32);
            }
        })
    });

    group.bench_function("from_str", |b| {
        b.iter(|| black_box(Event::from_str(json).unwrap()))
    });

    group.bench_function("to_str", |b| b.iter(|| black_box(event.to_string())));

    let index_event = event.index();
    let index_bytes = event.index().to_bytes().unwrap();
    // println!("index bytes len: {}", index_bytes.len());
    group.bench_function("index_to_bytes", |b| {
        b.iter(|| black_box(index_event.to_bytes().unwrap()))
    });

    group.bench_function("index_from_bytes", |b| {
        b.iter(|| black_box(EventIndex::from_bytes(&index_bytes).unwrap()))
    });

    group.bench_function("from_zeroes", |b| {
        let e = EventIndex::from_zeroes(&index_bytes).unwrap();
        b.iter(|| black_box(e.id()))
    });
    group.finish();
}

criterion_group!(benches, bench_event);
criterion_main!(benches);
