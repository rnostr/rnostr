use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use nokv::lmdb::Transaction;
use nokv_bench::*;
use std::time::{Duration, Instant};

fn bench_scanner1(c: &mut Criterion) {
    bench_scanner(c, 1_000_000, 10_000);
}

fn bench_scanner(c: &mut Criterion, init_len: usize, chunk_size: usize) {
    let num_str = fmt_num(init_len as f64);

    println!("Generate initial data {}", num_str);
    let now = Instant::now();
    let initial = gen_pairs(8, 8, init_len);
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
    let db = nokv::lmdb::Db::open_with(dir.path(), Some(30), Some(1_000), Some(1_000_000_000_000))
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
                    black_box(kv);
                    _total += 1;
                }
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scanner1);
criterion_main!(benches);
