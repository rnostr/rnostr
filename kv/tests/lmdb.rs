use anyhow::Result;
use nostr_kv::lmdb::{ffi, Db, Transaction};
use std::ops::{Bound, Deref};

#[test]
pub fn test_txn() -> Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("nokv-test-lmdb-txn")
        .tempdir()
        .unwrap();
    let db = Db::open(dir.path())?;
    let _t = db.open_tree(None, 0)?;
    let t1 = db.open_tree(Some("t1"), 0)?;

    {
        let mut writer = db.writer()?;
        writer.put(&t1, b"k1", b"v1")?;
        {
            let _iter = writer.iter(&t1);
        }
        let c = writer.commit();
        assert!(c.is_ok());
    }

    {
        let mut writer = db.writer()?;
        writer.put(&t1, b"k1", b"v1")?;
        {
            let _iter = writer.iter(&t1);
        }
        let c = writer.commit();
        assert!(c.is_ok());
    }

    {
        let mut writer = db.writer()?;
        writer.put(&t1, b"k2", b"v2")?;
        {
            assert_eq!(writer.get(&t1, "k2")?.unwrap(), b"v2");
        }
        {
            let reader = db.reader()?;
            assert!(reader.get(&t1, "k2")?.is_none());
        }
        let c = writer.commit();
        {
            let reader = db.reader()?;
            assert_eq!(reader.get(&t1, "k2")?.unwrap(), b"v2");
        }
        assert!(c.is_ok());
    }

    {
        let mut writer = db.writer()?;
        writer.put(&t1, b"k3", b"v3")?;
        writer.put(&t1, b"k31", b"v31")?;
        writer.commit()?;

        let reader = db.reader()?;
        let _v1 = reader.get(&t1, "k3")?.unwrap();
        drop(reader);
        let reader = db.reader()?;
        let _v2 = reader.get(&t1, "k31")?.unwrap();
        drop(reader);
        // println!("{:?} {:?}", _v1, _v2);
        // println!("{:?} {:?}", _v1, _v2);
    }

    Ok(())
}

#[test]
pub fn test_put_get_del() -> Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("nokv-test-lmdb-put-get-del")
        .tempdir()
        .unwrap();
    let db = Db::open(dir.path())?;
    let t = db.open_tree(None, 0)?;
    let t1 = db.open_tree(Some("t1"), 0)?;

    for tree in [&t, &t1] {
        let mut writer = db.writer()?;
        writer.put(tree, b"k1", b"v1")?;
        writer.put(tree, b"k2", b"v2")?;
        // update
        writer.put(tree, b"k2", b"v22")?;
        writer.put(tree, b"k3", b"v22")?;
        writer.del(tree, b"k3", None)?;
        writer.commit()?;

        {
            let reader = db.reader()?;
            assert_eq!(reader.get(tree, "k1")?.unwrap(), b"v1");
            assert_eq!(reader.get(tree, "k2")?.unwrap(), b"v22");
            assert!(reader.get(tree, "k3")?.is_none());
        }

        let mut writer = db.writer()?;
        writer.del(tree, "k2", None)?;
        writer.del(tree, "k22", None)?;
        writer.commit()?;
        {
            let reader = db.reader()?;
            assert!(reader.get(tree, "k2")?.is_none());
        }
    }

    let mut writer = db.writer()?;
    writer.put(&t1, b"exist", b"ok")?;
    writer.commit()?;
    {
        let reader = db.reader()?;
        assert_eq!(reader.get(&t1, "exist")?.unwrap(), b"ok");
    }

    db.drop_tree(Some("t1"))?;
    let t1 = db.open_tree(Some("t1"), 0)?;
    {
        let reader = db.reader()?;
        assert!(reader.get(&t1, "exist")?.is_none());
    }

    Ok(())
}

macro_rules! next_key {
    ($iter:ident) => {
        $iter.next().unwrap().unwrap().0.to_vec()
    };
}

#[test]
pub fn test_iter() -> Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("nokv-test-lmdb-iter")
        .tempdir()
        .unwrap();
    let db = Db::open(dir.path())?;
    // let t = db.open_tree(None, 0)?;
    let t1 = db.open_tree(Some("t1"), 0)?;
    let t2 = db.open_tree(Some("t2"), 0)?;

    // lmdb save the tree name to the default no name db, the test will not pass
    for tree in [&t1, &t2] {
        let mut writer = db.writer()?;
        writer.put(tree, b"k1", b"v1")?;
        writer.put(tree, b"k3", b"v3")?;
        writer.put(tree, b"k2", b"v2")?;
        writer.put(tree, b"k5", b"v5")?;
        writer.commit()?;

        let reader = db.reader()?;

        let mut iter = reader.iter(tree);
        assert_eq!(next_key!(iter), b"k1");
        assert_eq!(next_key!(iter), b"k2");
        assert_eq!(next_key!(iter), b"k3");
        assert_eq!(next_key!(iter), b"k5");
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(tree, Bound::Unbounded::<Vec<u8>>, true);
        assert_eq!(next_key!(iter), b"k5");
        assert_eq!(next_key!(iter), b"k3");
        assert_eq!(next_key!(iter), b"k2");
        assert_eq!(next_key!(iter), b"k1");
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(tree, Bound::Included("k3"), false);
        assert_eq!(next_key!(iter), b"k3");
        assert_eq!(next_key!(iter), b"k5");
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(tree, Bound::Excluded("k3"), false);
        assert_eq!(next_key!(iter), b"k5");
        assert!(iter.next().is_none());

        // rev
        let mut iter = reader.iter_from(tree, Bound::Included("k2"), true);
        assert_eq!(next_key!(iter), b"k2");
        assert_eq!(next_key!(iter), b"k1");
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(tree, Bound::Excluded("k2"), true);
        assert_eq!(next_key!(iter), b"k1");
        assert!(iter.next().is_none());

        // Non-existent key
        let mut iter = reader.iter_from(tree, Bound::Included("k4"), false);
        assert_eq!(next_key!(iter), b"k5");
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(tree, Bound::Excluded("k4"), false);
        assert_eq!(next_key!(iter), b"k5");
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(tree, Bound::Included("k4"), true);
        assert_eq!(next_key!(iter), b"k3");
        assert_eq!(next_key!(iter), b"k2");
        assert_eq!(next_key!(iter), b"k1");
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(tree, Bound::Excluded("k4"), true);
        assert_eq!(next_key!(iter), b"k3");
        assert_eq!(next_key!(iter), b"k2");
        assert_eq!(next_key!(iter), b"k1");
        assert!(iter.next().is_none());

        // Out of range
        let mut iter = reader.iter_from(tree, Bound::Included("k0"), false);
        assert_eq!(next_key!(iter), b"k1");

        let mut iter = reader.iter_from(tree, Bound::Included("k8"), true);
        assert_eq!(next_key!(iter), b"k5");

        // seek
        let mut iter = reader.iter(tree);
        iter.seek(Bound::Excluded("k4"), true);
        assert_eq!(next_key!(iter), b"k3");
        assert_eq!(next_key!(iter), b"k2");
        assert_eq!(next_key!(iter), b"k1");
        assert!(iter.next().is_none());
    }

    Ok(())
}

#[test]
pub fn test_dup() -> Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("nokv-test-lmdb-dup")
        .tempdir()
        .unwrap();
    let db = Db::open(dir.path())?;

    let tree = db.open_tree(Some("t1"), ffi::MDB_DUPSORT)?;

    let mut writer = db.writer()?;
    let one = 1u64.to_be_bytes().to_vec();
    let two = 2u64.to_be_bytes().to_vec();
    let three = 3u64.to_be_bytes().to_vec();
    let ext = b"ext".to_vec();
    let one_ext: Vec<u8> = [one.deref(), &ext].concat();
    let two_ext: Vec<u8> = [two.deref(), &ext].concat();

    writer.put(&tree, b"i1", &one)?;
    writer.put(&tree, b"i1", &two)?;
    writer.put(&tree, b"i1", &two)?; // repeat
    writer.put(&tree, b"i1", &three)?;
    writer.del(&tree, b"i1", Some(&three))?; // del
    writer.put(&tree, b"k3", &one_ext)?;
    writer.put(&tree, b"k3", &two_ext)?;

    writer.put(&tree, b"i4", &one)?;
    writer.put(&tree, b"i4", &two)?;
    writer.commit()?;

    let mut writer = db.writer()?;
    // writer.del(&tree, b"i4", None)?; // rocksdb will error
    writer.del(&tree, b"i4", Some(&one))?;
    writer.del(&tree, b"i4", Some(&two))?;
    writer.commit()?;

    {
        let reader = db.reader()?;

        let mut iter = reader.iter_from(&tree, Bound::Unbounded::<Vec<u8>>, false);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, one);

        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, two);

        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, one_ext);

        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, two_ext);
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(&tree, Bound::Unbounded::<Vec<u8>>, true);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, two_ext);

        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, one_ext);

        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, two);

        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, one);
        assert!(iter.next().is_none());

        let mut iter = reader.iter_from(&tree, Bound::Included("k3"), false);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, one_ext);

        let mut iter = reader.iter_from(&tree, Bound::Included("i2"), true);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, two);

        let mut iter = reader.iter_from(&tree, Bound::Excluded("i1"), false);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, one_ext);

        let mut iter = reader.iter_from(&tree, Bound::Excluded("k3"), true);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, two);

        let mut iter = reader.iter_from(&tree, Bound::Included("i2"), false);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, one_ext);

        let mut iter = reader.iter_from(&tree, Bound::Included("i2"), true);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, two);

        iter.seek(Bound::Included("i1"), false);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, one);

        iter.seek(Bound::Included("i1"), true);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, two);

        iter.seek(Bound::Included("k"), true);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"i1");
        assert_eq!(item.1, two);

        iter.seek(Bound::Included("m"), true);
        let item = iter.next().unwrap().unwrap();
        assert_eq!(item.0, b"k3");
        assert_eq!(item.1, two_ext);
    }

    {
        let i_tree = db.open_tree(
            Some("t2"),
            ffi::MDB_DUPSORT | ffi::MDB_DUPFIXED | ffi::MDB_INTEGERKEY | ffi::MDB_INTEGERDUP,
        )?;
        let mut writer = db.writer()?;
        let mut c = 0u64;
        for i in 0u64..30 {
            writer.put(&i_tree, i.to_be_bytes(), c.to_be_bytes())?;
            c += 1;
            writer.put(&i_tree, i.to_be_bytes(), c.to_be_bytes())?;
            c += 1;
        }
        writer.commit()?;
        let reader = db.reader()?;
        let iter = reader.iter_from(&i_tree, Bound::Unbounded::<Vec<u8>>, false);
        assert_eq!(iter.count(), 60);

        let iter = reader.iter_from(&i_tree, Bound::Included(10u64.to_be_bytes()), false);
        let iter1 = reader.iter_from(&i_tree, Bound::Included(20u64.to_be_bytes()), false);
        assert_eq!(iter.count(), 40);
        assert_eq!(iter1.count(), 20);
    }
    Ok(())
}
