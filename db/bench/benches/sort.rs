use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use nostr_kv_bench::gen_pairs;
use rand::Rng;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

fn bench_sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort");
    group.measurement_time(Duration::from_secs(1));
    group.sample_size(50);
    group.warm_up_time(Duration::from_millis(100));
    group.throughput(Throughput::Elements(1));

    let mut rng = rand::thread_rng();

    group.bench_function("gen", |b| {
        b.iter(|| black_box(rng.gen::<u64>().to_be_bytes().to_vec()))
    });

    let pairs = gen_pairs(33, 33, 1000);

    group.bench_function("clone", |b| b.iter(|| black_box(pairs.clone())));

    group.bench_function("sort", |b| {
        b.iter(|| black_box(pairs.clone().sort_by(|a, b| a.0.cmp(&b.0))))
    });
    group.bench_function("max", |b| {
        b.iter(|| black_box(pairs.iter().max_by(|a, b| a.0.cmp(&b.0))))
    });

    let mut list = (0..3000)
        .map(|_| rng.gen::<u64>().to_be_bytes().to_vec())
        .collect::<Vec<_>>();
    list.sort();

    group.bench_function("sorted ver with pop last", |b| {
        b.iter(|| {
            let el = rng.gen::<u64>().to_be_bytes().to_vec();
            let insert_at = match list.binary_search_by(|p| p.cmp(&el)) {
                Ok(insert_at) | Err(insert_at) => insert_at,
            };
            list.insert(insert_at, el);
            black_box(list.pop().unwrap());
        })
    });
    group.bench_function("sorted ver with remove first", |b| {
        b.iter_batched(
            || rng.gen::<u64>().to_be_bytes().to_vec(),
            |el| {
                let insert_at = match list.binary_search_by(|p| p.cmp(&el)) {
                    Ok(insert_at) | Err(insert_at) => insert_at,
                };
                list.insert(insert_at, el);
                black_box(list.remove(0));
            },
            BatchSize::SmallInput,
        );
        // b.iter(|| {
        //     let el: Vec<u8> = rng.gen::<u64>().to_be_bytes().to_vec();

        // })
    });

    let mut list = VecDeque::from(list);
    group.bench_function("sorted deque with pop last", |b| {
        b.iter(|| {
            let el = rng.gen::<u64>().to_be_bytes().to_vec();
            let insert_at = match list.binary_search_by(|p| p.cmp(&el)) {
                Ok(insert_at) | Err(insert_at) => insert_at,
            };
            list.insert(insert_at, el);
            black_box(list.pop_back().unwrap());
        })
    });
    group.bench_function("sorted deque with remove first", |b| {
        b.iter(|| {
            let el = rng.gen::<u64>().to_be_bytes().to_vec();
            let insert_at = match list.binary_search_by(|p| p.cmp(&el)) {
                Ok(insert_at) | Err(insert_at) => insert_at,
            };
            list.insert(insert_at, el);
            black_box(list.pop_front().unwrap());
        })
    });

    let mut list = (0..3000)
        .map(|_| rng.gen::<u64>().to_be_bytes().to_vec())
        .collect::<Vec<_>>();
    list.sort();
    group.bench_function("custom bench - sorted ver with pop last", |b| {
        b.iter_custom(|iters| {
            let els = (0..iters)
                .map(|_| rng.gen::<u64>().to_be_bytes().to_vec())
                .collect::<Vec<_>>();
            let start = Instant::now();
            for el in els {
                let insert_at = match list.binary_search_by(|p| p.cmp(&el)) {
                    Ok(insert_at) | Err(insert_at) => insert_at,
                };
                list.insert(insert_at, el);
                black_box(list.pop().unwrap());
            }
            start.elapsed()
        });
    });
    group.finish();
}

criterion_group!(benches, bench_sort);
criterion_main!(benches);
