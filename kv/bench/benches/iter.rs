use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use lmdb::{Cursor, Transaction};
use nostr_kv::lmdb::Transaction as Txn;
use nostr_kv_bench::*;
use std::time::{Duration, Instant};

fn bench_put_get1(c: &mut Criterion) {
    bench_put_get(c, 1_000_000, 10_000);
}

fn bench_put_get(c: &mut Criterion, init_len: usize, chunk_size: usize) {
    let num_str = fmt_num(init_len as f64);

    println!("Generate initial data {}", num_str);
    let now = Instant::now();
    let initial = gen_pairs(8, 8, init_len);
    println!("Generated in {:?}", now.elapsed());
    let initial_chunks = chunk_vec(&initial, chunk_size);

    let mut group = c.benchmark_group(format!("put-get-{}-{}", num_str, chunk_size));
    group.measurement_time(Duration::from_secs(3));
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.throughput(Throughput::Elements(init_len as u64));

    {
        let dir = tempfile::Builder::new()
            .prefix("nokv-bench-put-get-lmdb")
            .tempdir()
            .unwrap();
        let db = nostr_kv::lmdb::Db::open_with(
            dir.path(),
            Some(30),
            Some(1_000),
            Some(1_000_000_000_000),
            0,
        )
        .unwrap();
        let tree = db.open_tree(Some("t1"), 0).unwrap();

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

        let reader = db.reader().unwrap();
        group.bench_function("lmdb-all", |b| {
            b.iter(|| {
                let mut iter = reader.iter(&tree);
                black_box(&iter);
                while let Some(kv) = iter.next() {
                    black_box(kv.unwrap());
                }
            })
        });
    }

    {
        let dir = tempfile::Builder::new()
            .prefix("nokv-bench-put-get-lmdb-rkv")
            .tempdir()
            .unwrap();
        let env = lmdb::Environment::new()
            .set_max_dbs(30)
            .set_max_readers(1_000)
            .set_map_size(1_000_000_000_000)
            .open(dir.path())
            .unwrap();
        let db = env
            .create_db(Some("t1"), lmdb::DatabaseFlags::empty())
            .unwrap();

        println!("lmdb-rkv: Put initial data batch {}", chunk_size);
        let now = Instant::now();
        for chunk in initial_chunks.iter() {
            let mut txn = env.begin_rw_txn().unwrap();
            for (k, v) in chunk {
                txn.put(db, k, v, lmdb::WriteFlags::empty()).unwrap();
            }
            txn.commit().unwrap();
        }
        println!(
            "put in {:?} {:?}",
            now.elapsed(),
            fmt_per_sec(init_len, &now.elapsed())
        );

        let ro = env.begin_ro_txn().unwrap();
        group.bench_function("lmdb-rkv-all", |b| {
            b.iter(|| {
                let mut cursor = ro.open_ro_cursor(db).unwrap();
                let mut iter = cursor.iter_start();
                black_box(&iter);
                while let Some(kv) = iter.next() {
                    black_box(kv.unwrap());
                }
            })
        });
    }

    group.finish();
}

fn bench_create(c: &mut Criterion) {
    let mut group = c.benchmark_group("create");
    group.measurement_time(Duration::from_secs(1));
    group.sample_size(50);
    group.warm_up_time(Duration::from_millis(100));
    group.throughput(Throughput::Elements(1));

    {
        let dir = tempfile::Builder::new()
            .prefix("nokv-bench-create-lmdb")
            .tempdir()
            .unwrap();
        let db = nostr_kv::lmdb::Db::open_with(
            dir.path(),
            Some(30),
            Some(1_000),
            Some(1_000_000_000_000),
            0,
        )
        .unwrap();
        let tree = db.open_tree(None, 0).unwrap();
        group.bench_function("lmdb", |b| {
            b.iter(|| {
                let reader = db.reader().unwrap();
                let iter = reader.iter_from(&tree, std::ops::Bound::Included("key"), false);
                black_box(iter);
            })
        });
    }

    {
        let dir = tempfile::Builder::new()
            .prefix("nokv-bench-create-lmdb-rkv")
            .tempdir()
            .unwrap();
        let env = lmdb::Environment::new()
            .set_max_dbs(30)
            .set_max_readers(1_000)
            .set_map_size(1_000_000_000_000)
            .open(dir.path())
            .unwrap();
        let db = env.open_db(None).unwrap();
        group.bench_function("lmdb-rkv", |b| {
            b.iter(|| {
                let ro = env.begin_ro_txn().unwrap();
                let mut cursor = ro.open_ro_cursor(db).unwrap();
                let iter = cursor.iter_from("key");
                black_box(iter)
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_create, bench_put_get1);
criterion_main!(benches);
