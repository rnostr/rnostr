use crate::Error;
use libc::{c_char, c_int, c_uint, c_void, size_t, EINVAL};
pub use lmdb_master_sys as ffi;
use parking_lot::RwLock;
use std::{
    cmp::Ordering,
    collections::HashMap,
    ffi::{CStr, CString},
    fs,
    marker::PhantomData,
    mem::{self, MaybeUninit},
    ops::{Bound, Deref},
    path::Path,
    ptr, slice,
    sync::Arc,
};

macro_rules! lmdb_try {
    ($expr:expr) => {{
        match $expr {
            ffi::MDB_SUCCESS => (),
            err_code => return Err(lmdb_error(err_code)),
        }
    }};
}

macro_rules! lmdb_try_with_cleanup {
    ($expr:expr, $cleanup:expr) => {{
        match $expr {
            ffi::MDB_SUCCESS => (),
            err_code => {
                let _ = $cleanup;
                return Err(lmdb_error(err_code));
            }
        }
    }};
}

type Result<T, E = Error> = core::result::Result<T, E>;

struct Dbi {
    inner: ffi::MDB_dbi,
}

impl Dbi {
    fn new(txn: *mut ffi::MDB_txn, name: Option<&str>, flags: c_uint) -> Result<Self> {
        let c_name = name.map(CString::new).transpose()?;
        let name_ptr = if let Some(ref c_name) = c_name {
            c_name.as_ptr()
        } else {
            ptr::null()
        };
        let mut dbi: ffi::MDB_dbi = 0;
        unsafe {
            lmdb_result(ffi::mdb_dbi_open(txn, name_ptr, flags, &mut dbi))?;
        }
        Ok(Self { inner: dbi })
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    inner: ffi::MDB_dbi,
    flags: c_uint,
}

unsafe impl Send for Tree {}
unsafe impl Sync for Tree {}

pub trait Transaction: Sized {
    fn txn(&self) -> *mut ffi::MDB_txn;

    fn commit(self) -> Result<()> {
        unsafe {
            let result = lmdb_result(ffi::mdb_txn_commit(self.txn()));
            mem::forget(self);
            result
        }
    }

    fn get<'txn, K: AsRef<[u8]>>(&'txn self, tree: &Tree, key: K) -> Result<Option<&'txn [u8]>> {
        let key = key.as_ref();
        let mut key_val = ffi::MDB_val {
            mv_size: key.len() as size_t,
            mv_data: key.as_ptr() as *mut c_void,
        };

        let mut data_val = MaybeUninit::uninit();
        unsafe {
            match ffi::mdb_get(self.txn(), tree.inner, &mut key_val, data_val.as_mut_ptr()) {
                ffi::MDB_SUCCESS => Ok(Some(val_to_slice(data_val.assume_init()))),
                ffi::MDB_NOTFOUND => Ok(None),
                err_code => Err(lmdb_error(err_code)),
            }
        }
    }

    fn iter_from<'txn, K: AsRef<[u8]>>(
        &'txn self,
        tree: &Tree,
        from: Bound<K>,
        rev: bool,
    ) -> Iter<'txn> {
        let mut iter = Iter::new(self, tree);
        iter.seek(from, rev);
        iter
    }

    fn iter(&self, tree: &Tree) -> Iter {
        self.iter_from(tree, Bound::Unbounded::<Vec<u8>>, false)
    }
}

pub struct Reader<'env> {
    inner: *mut ffi::MDB_txn,
    _marker: PhantomData<&'env Db>,
}

impl<'env> Drop for Reader<'env> {
    fn drop(&mut self) {
        unsafe { ffi::mdb_txn_abort(self.inner) }
    }
}

impl<'env> Transaction for Reader<'env> {
    fn txn(&self) -> *mut ffi::MDB_txn {
        self.inner
    }
}

impl<'env> Reader<'env> {
    fn new(db: &'env DbInner) -> Result<Self> {
        let mut txn: *mut ffi::MDB_txn = ptr::null_mut();
        unsafe {
            lmdb_result(ffi::mdb_txn_begin(
                db.inner,
                ptr::null_mut(),
                ffi::MDB_RDONLY,
                &mut txn,
            ))?;
        }
        Ok(Self {
            inner: txn,
            _marker: PhantomData,
        })
    }
}

pub struct Writer<'env> {
    inner: *mut ffi::MDB_txn,
    _marker: PhantomData<&'env Db>,
}

impl<'env> Drop for Writer<'env> {
    fn drop(&mut self) {
        unsafe { ffi::mdb_txn_abort(self.inner) }
    }
}

impl<'env> Transaction for Writer<'env> {
    fn txn(&self) -> *mut ffi::MDB_txn {
        self.inner
    }
}

impl<'env> Writer<'env> {
    fn new(db: &'env DbInner) -> Result<Self> {
        let mut txn: *mut ffi::MDB_txn = ptr::null_mut();
        unsafe {
            lmdb_result(ffi::mdb_txn_begin(db.inner, ptr::null_mut(), 0, &mut txn))?;
        }
        Ok(Self {
            inner: txn,
            _marker: PhantomData,
        })
    }

    pub fn put<K, V>(&mut self, tree: &Tree, key: K, value: V) -> Result<()>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let flags = 0;
        let key = key.as_ref();
        let value = value.as_ref();

        let mut key_val: ffi::MDB_val = ffi::MDB_val {
            mv_size: key.len() as size_t,
            mv_data: key.as_ptr() as *mut c_void,
        };
        let mut data_val: ffi::MDB_val = ffi::MDB_val {
            mv_size: value.len() as size_t,
            mv_data: value.as_ptr() as *mut c_void,
        };
        unsafe {
            lmdb_result(ffi::mdb_put(
                self.inner,
                tree.inner,
                &mut key_val,
                &mut data_val,
                flags,
            ))
        }
    }

    pub fn del<K: AsRef<[u8]>>(&mut self, tree: &Tree, key: K, value: Option<&[u8]>) -> Result<()> {
        let key = key.as_ref();
        let mut key_val: ffi::MDB_val = ffi::MDB_val {
            mv_size: key.len() as size_t,
            mv_data: key.as_ptr() as *mut c_void,
        };

        if let Some(value) = value {
            let mut data_val = ffi::MDB_val {
                mv_size: value.len() as size_t,
                mv_data: value.as_ptr() as *mut c_void,
            };
            unsafe {
                match ffi::mdb_del(self.inner, tree.inner, &mut key_val, &mut data_val) {
                    ffi::MDB_SUCCESS | ffi::MDB_NOTFOUND => Ok(()),
                    err_code => Err(lmdb_error(err_code)),
                }
            }
        } else {
            unsafe {
                match ffi::mdb_del(self.inner, tree.inner, &mut key_val, ptr::null_mut()) {
                    ffi::MDB_SUCCESS | ffi::MDB_NOTFOUND => Ok(()),
                    err_code => Err(lmdb_error(err_code)),
                }
            }
        }
    }
}

fn to_cpath<P: AsRef<Path>>(path: P) -> Result<CString, Error> {
    Ok(CString::new(path.as_ref().to_string_lossy().as_bytes())?)
}

struct DbInner {
    inner: *mut ffi::MDB_env,
    dbs: RwLock<HashMap<Option<String>, Dbi>>,
}

impl Drop for DbInner {
    fn drop(&mut self) {
        unsafe { ffi::mdb_env_close(self.inner) }
    }
}

impl DbInner {
    fn open<P: AsRef<Path>>(
        path: P,
        maxdbs: Option<u32>,
        maxreaders: Option<u32>,
        mapsize: Option<usize>,
        flag: u32,
    ) -> Result<Self> {
        // let flag = ffi::MDB_NOTLS;

        let path = path.as_ref();
        let c_path = to_cpath(path)?;

        if let Err(e) = fs::create_dir_all(path) {
            return Err(Error::Message(format!(
                "Failed to create LMDB directory: `{e:?}`."
            )));
        }

        let mut env: *mut ffi::MDB_env = ptr::null_mut();
        unsafe {
            lmdb_try!(ffi::mdb_env_create(&mut env));

            if let Some(maxdbs) = maxdbs {
                lmdb_try_with_cleanup!(
                    ffi::mdb_env_set_maxdbs(env, maxdbs),
                    ffi::mdb_env_close(env)
                );
            }

            if let Some(maxreaders) = maxreaders {
                lmdb_try_with_cleanup!(
                    ffi::mdb_env_set_maxreaders(env, maxreaders),
                    ffi::mdb_env_close(env)
                );
            }

            if let Some(mapsize) = mapsize {
                lmdb_try_with_cleanup!(
                    ffi::mdb_env_set_mapsize(env, mapsize),
                    ffi::mdb_env_close(env)
                );
            }

            lmdb_try_with_cleanup!(
                ffi::mdb_env_open(env, c_path.as_ptr(), flag, 0o644),
                ffi::mdb_env_close(env)
            );
        }

        Ok(Self {
            inner: env,
            dbs: RwLock::new(HashMap::new()),
        })
    }

    fn open_tree(&self, name: Option<&str>, flags: c_uint) -> Result<Tree> {
        let sname = name.map(ToOwned::to_owned);
        {
            let dbs = self.dbs.read();
            if let Some(dbi) = dbs.get(&sname) {
                return Ok(Tree {
                    flags,
                    inner: dbi.inner,
                });
            }
        }

        // we need to check this again in case another
        // thread opened it concurrently.
        let mut dbs = self.dbs.write();
        if let Some(dbi) = dbs.get(&sname) {
            return Ok(Tree {
                flags,
                inner: dbi.inner,
            });
        }

        // create
        let writer = Writer::new(self)?;
        let flags = ffi::MDB_CREATE | flags;

        let dbi = Dbi::new(writer.inner, name, flags)?;
        let inner = dbi.inner;
        writer.commit()?;
        dbs.insert(sname, dbi);
        Ok(Tree { flags, inner })
    }

    fn drop_tree(&self, name: Option<&str>) -> Result<bool> {
        if let Some(dbi) = self.dbs.write().remove(&name.map(ToOwned::to_owned)) {
            let writer = Writer::new(self)?;
            unsafe {
                lmdb_result(ffi::mdb_drop(writer.inner, dbi.inner, 1))?;
            }
            writer.commit()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[derive(Clone)]
pub struct Db {
    inner: Arc<DbInner>,
}

unsafe impl Send for DbInner {}
unsafe impl Sync for DbInner {}

impl Db {
    pub fn writer(&self) -> Result<Writer> {
        Writer::new(&self.inner)
    }

    pub fn open_tree(&self, name: Option<&str>, flags: c_uint) -> std::result::Result<Tree, Error> {
        self.inner.open_tree(name, flags)
    }

    pub fn drop_tree(&self, name: Option<&str>) -> std::result::Result<bool, Error> {
        self.inner.drop_tree(name)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with(path, Some(20), Some(100), Some(1_000_000_000_000), 0)
    }

    pub fn open_with<P: AsRef<Path>>(
        path: P,
        maxdbs: Option<u32>,
        maxreaders: Option<u32>,
        mapsize: Option<usize>,
        flags: u32,
    ) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(DbInner::open(path, maxdbs, maxreaders, mapsize, flags)?),
        })
    }

    pub fn reader(&self) -> Result<Reader> {
        Reader::new(&self.inner)
    }

    pub fn flush(&self) -> Result<()> {
        unsafe {
            lmdb_result(ffi::mdb_env_sync(self.inner.inner, 1))?;
        }
        Ok(())
    }
}

pub struct Iter<'txn> {
    err: Option<Error>,
    inner: Option<IterInner<'txn>>,
    rev: bool,
    op: c_uint,
    next_op: c_uint,
    dup: bool,
}

impl<'txn> Iter<'txn> {
    fn new<T: Transaction>(txn: &'txn T, tree: &Tree) -> Self {
        let dup = tree.flags & ffi::MDB_DUPSORT == ffi::MDB_DUPSORT;

        let inner = IterInner::new(txn, tree.inner);
        match inner {
            Err(err) => Self {
                err: Some(err),
                inner: None,
                rev: false,
                op: 0,
                next_op: 0,
                dup,
            },
            Ok(inner) => Self {
                err: None,
                inner: Some(inner),
                rev: false,
                op: 0,
                next_op: 0,
                dup,
            },
        }
    }
}

impl<'txn> Iter<'txn> {
    pub fn seek<K: AsRef<[u8]>>(&mut self, from: Bound<K>, rev: bool) {
        self.rev = rev;
        if let Some(ref mut inner) = self.inner {
            if rev {
                self.next_op = ffi::MDB_PREV;
                match from {
                    Bound::Included(start) => {
                        self.op = ffi::MDB_GET_CURRENT;
                        match inner.get_by_key(start.as_ref(), ffi::MDB_SET_RANGE) {
                            Ok(Some((key, _))) => {
                                let cmp = key.deref().cmp(start.as_ref());
                                match cmp {
                                    Ordering::Greater => {
                                        self.op = ffi::MDB_PREV;
                                    }
                                    Ordering::Equal if self.dup => {
                                        // move to last value if the same key
                                        // MDB_LAST_DUP will not return key
                                        // self.op = ffi::MDB_LAST_DUP;
                                        let _r = inner.get(ffi::MDB_LAST_DUP);
                                    }
                                    _ => {}
                                };
                            }
                            Ok(None) => {
                                // bigger than all
                                self.op = ffi::MDB_LAST;
                            }
                            Err(err) => {
                                self.err = Some(err);
                                self.inner = None;
                            }
                        }
                    }
                    Bound::Excluded(start) => {
                        self.op = ffi::MDB_GET_CURRENT;
                        match inner.get_by_key(start.as_ref(), ffi::MDB_SET_RANGE) {
                            Ok(Some((key, _))) => {
                                if key >= start.as_ref() {
                                    if self.dup {
                                        self.op = ffi::MDB_PREV_NODUP;
                                    } else {
                                        self.op = ffi::MDB_PREV;
                                    }
                                }
                            }
                            Ok(None) => {
                                // bigger than all
                                self.op = ffi::MDB_LAST;
                            }
                            Err(err) => {
                                self.err = Some(err);
                                self.inner = None;
                            }
                        }
                    }
                    Bound::Unbounded => {
                        self.op = ffi::MDB_LAST;
                    }
                };
            } else {
                self.next_op = ffi::MDB_NEXT;
                match from {
                    Bound::Included(start) => {
                        self.op = ffi::MDB_GET_CURRENT;
                        if let Err(err) = inner.get_by_key(start.as_ref(), ffi::MDB_SET_RANGE) {
                            self.err = Some(err);
                            self.inner = None;
                        }
                    }
                    Bound::Excluded(start) => {
                        self.op = ffi::MDB_GET_CURRENT;
                        match inner.get_by_key(start.as_ref(), ffi::MDB_SET_RANGE) {
                            Ok(Some((key, _))) => {
                                if start.as_ref() == key {
                                    if self.dup {
                                        self.op = ffi::MDB_NEXT_NODUP;
                                    } else {
                                        self.op = ffi::MDB_NEXT;
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                self.err = Some(err);
                                self.inner = None;
                            }
                        }
                    }
                    Bound::Unbounded => {
                        self.op = ffi::MDB_FIRST;
                    }
                };
            }
        }
    }
}

impl<'txn> Iterator for Iter<'txn> {
    type Item = Result<(&'txn [u8], &'txn [u8]), Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(ref mut inner) = self.inner {
            let op = mem::replace(&mut self.op, self.next_op);
            let item = inner.get(op);
            // self.op = self.next_op;
            item.transpose()
        } else {
            self.err.as_ref().map(|err| Err(err.clone()))
        }
    }
}

fn lmdb_error(err_code: c_int) -> Error {
    unsafe {
        // This is safe since the error messages returned from mdb_strerror are static.
        let err: *const c_char = ffi::mdb_strerror(err_code) as *const c_char;
        Error::Lmdb(std::str::from_utf8_unchecked(CStr::from_ptr(err).to_bytes()).to_string())
    }
}

fn lmdb_result(err_code: c_int) -> Result<()> {
    if err_code == ffi::MDB_SUCCESS {
        Ok(())
    } else {
        Err(lmdb_error(err_code))
    }
}

unsafe fn val_to_slice<'a>(val: ffi::MDB_val) -> &'a [u8] {
    slice::from_raw_parts(val.mv_data as *const u8, val.mv_size)
}

struct IterInner<'txn> {
    _marker: PhantomData<&'txn ()>,
    cursor: *mut ffi::MDB_cursor,
}

type Item<'a> = Result<Option<(&'a [u8], &'a [u8])>>;

impl<'txn> IterInner<'txn> {
    fn new<T: Transaction>(txn: &'txn T, dbi: ffi::MDB_dbi) -> Result<Self> {
        let mut cursor: *mut ffi::MDB_cursor = ptr::null_mut();
        unsafe {
            lmdb_result(ffi::mdb_cursor_open(txn.txn(), dbi, &mut cursor))?;
        }
        Ok(Self {
            cursor,
            _marker: PhantomData,
        })
    }

    fn get_by_key(&mut self, key: &[u8], op: c_uint) -> Item<'txn> {
        let mut key = ffi::MDB_val {
            mv_size: key.len() as size_t,
            mv_data: key.as_ptr() as *mut c_void,
        };
        let mut data = MaybeUninit::uninit();
        unsafe {
            match ffi::mdb_cursor_get(self.cursor, &mut key, data.as_mut_ptr(), op) {
                ffi::MDB_SUCCESS => Ok(Some((val_to_slice(key), val_to_slice(data.assume_init())))),
                // EINVAL can occur when the cursor was previously seeked to a non-existent value,
                // e.g. iter_from with a key greater than all values in the database.
                ffi::MDB_NOTFOUND | EINVAL => Ok(None),
                error => Err(lmdb_error(error)),
            }
        }
    }

    fn get(&mut self, op: c_uint) -> Item<'txn> {
        let mut key = MaybeUninit::uninit();
        let mut data = MaybeUninit::uninit();
        unsafe {
            match ffi::mdb_cursor_get(self.cursor, key.as_mut_ptr(), data.as_mut_ptr(), op) {
                ffi::MDB_SUCCESS => Ok(Some((
                    val_to_slice(key.assume_init()),
                    val_to_slice(data.assume_init()),
                ))),
                // EINVAL can occur when the cursor was previously seeked to a non-existent value,
                // e.g. iter_from with a key greater than all values in the database.
                ffi::MDB_NOTFOUND | EINVAL => Ok(None),
                error => Err(lmdb_error(error)),
            }
        }
    }
}

impl<'txn> Drop for IterInner<'txn> {
    fn drop(&mut self) {
        unsafe {
            ffi::mdb_cursor_close(self.cursor);
        }
    }
}
