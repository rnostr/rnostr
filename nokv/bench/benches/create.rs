use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use lmdb::{Cursor, Transaction};

fn bench_create_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("create iter");
    group.measurement_time(Duration::from_secs(1));
    group.sample_size(50);
    group.warm_up_time(Duration::from_millis(100));
    group.throughput(Throughput::Elements(1));

    {
        let dir = tempfile::Builder::new()
            .prefix("nokv-bench-create-lmdb")
            .tempdir()
            .unwrap();
        let db =
            nokv::lmdb::Db::open_with(dir.path(), Some(30), Some(1_000), Some(1_000_000_000_000))
                .unwrap();
        let tree = db.open_tree(None, 0).unwrap();
        group.bench_function("lmdb", |b| {
            b.iter(|| {
                let reader = db.reader().unwrap();
                let iter = reader.iter_from(&tree, std::ops::Bound::Included("key"), false);
                black_box(iter)
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

criterion_group!(benches, bench_create_iter);
criterion_main!(benches);
