//! CrabScheme stdlib module: `(crab store)` — RocksDB host-procedure binding.
//!
//! Exposes RocksDB as opaque fixnum handles, mirroring the `cs-stdlib-tls`
//! and `cs-stdlib-sql` native-only host-procedure pattern exactly.
//!
//! Native-only: RocksDB vendors C++ and cannot target wasm32. Excluded from
//! `wasm-stdlib`. Feature-gated as `stdlib-store` in `cs-runtime`.
//!
//! ## Handle model
//!
//! - DB handles: `i64` fixnums mapping into a process-global `DbRegistry`
//!   (`OnceLock<Mutex<DbRegistry>>`).
//! - Iterator handles: separate `IterRegistry` (`OnceLock<Mutex<IterRegistry>>`).
//!
//! ## Registered procedures
//!
//! | Scheme name         | Args                              | Returns           |
//! |---------------------|-----------------------------------|-------------------|
//! | `store-open`        | path [create-if-missing?]         | db-handle (i64)   |
//! | `store-close`       | db                                | unspecified       |
//! | `store-cf-create`   | db cf-name                        | unspecified       |
//! | `store-get`         | db cf key                         | bytevector \| #f  |
//! | `store-put`         | db cf key val [sync?]             | unspecified       |
//! | `store-delete`      | db cf key [sync?]                 | unspecified       |
//! | `store-write-batch` | db ops [sync?]                    | unspecified       |
//! | `store-iter`        | db cf prefix                      | iter-handle (i64) |
//! | `store-iter-next`   | iter                              | (key . val) \| #f |
//! | `store-iter-close`  | iter                              | unspecified       |
//! | `store-checkpoint`  | db dir                            | unspecified       |
//! | `store-flush`       | db                                | unspecified       |
//! | `store-flush-wal`   | db [sync?]                        | unspecified       |
//!
//! Keys and values are **bytevectors**. Column-family names are strings.
//! `sync?` is a boolean (default `#f`). `ops` for `store-write-batch` is a
//! Scheme proper list where each element is a list whose first element is the
//! string `"put"` or `"delete"`:
//!   `(list "put" cf key val)` or `(list "delete" cf key)`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::{Gc, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

use rocksdb::{
    checkpoint::Checkpoint, DBWithThreadMode, MultiThreaded, Options, WriteBatch, WriteOptions,
};

// MultiThreaded mode (cw-b5w.6): RocksDB is internally thread-safe, so the
// registry hands out Arc<RocksDb> clones and every operation runs WITHOUT the
// registry Mutex held — concurrent apply workers' reads/writes proceed in
// parallel (the old SingleThreaded-behind-a-Mutex setup re-serialized them,
// which is what made --shards>1 a net loss).
type RocksDb = DBWithThreadMode<MultiThreaded>;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("store-open", store_open),
        UntypedProc::new("store-close", store_close),
        UntypedProc::new("store-cf-create", store_cf_create),
        UntypedProc::new("store-get", store_get),
        UntypedProc::new("store-put", store_put),
        UntypedProc::new("store-put-many", store_put_many),
        UntypedProc::new("store-delete", store_delete),
        UntypedProc::new("store-write-batch", store_write_batch),
        UntypedProc::new("store-iter", store_iter),
        UntypedProc::new("store-iter-range", store_iter_range),
        UntypedProc::new("store-range-latest-pb", store_range_latest_pb),
        UntypedProc::new("store-iter-next", store_iter_next),
        UntypedProc::new("store-iter-close", store_iter_close),
        UntypedProc::new("store-seek", store_seek),
        UntypedProc::new("store-checkpoint", store_checkpoint),
        UntypedProc::new("store-flush", store_flush),
        UntypedProc::new("store-flush-wal", store_flush_wal),
    ]
}

// ---- DB registry -------------------------------------------------------

struct DbRegistry {
    next_id: i64,
    slots: HashMap<i64, Arc<RocksDb>>,
}

fn db_registry() -> &'static Mutex<DbRegistry> {
    static R: OnceLock<Mutex<DbRegistry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(DbRegistry {
            next_id: 1,
            slots: HashMap::new(),
        })
    })
}

fn db_lock() -> Result<std::sync::MutexGuard<'static, DbRegistry>, FfiError> {
    db_registry()
        .lock()
        .map_err(|e| FfiError::HostFailure(format!("store: db registry poisoned: {}", e)))
}

fn db_insert(db: RocksDb) -> Result<i64, FfiError> {
    let mut r = db_lock()?;
    let id = r.next_id;
    r.next_id += 1;
    r.slots.insert(id, Arc::new(db));
    Ok(id)
}

/// Clone the Arc handle out under a BRIEF registry lock; the DB operation
/// itself then runs unlocked (MultiThreaded RocksDB is internally safe).
fn db_get(id: i64, who: &str) -> Result<Arc<RocksDb>, FfiError> {
    let r = db_lock()?;
    r.slots
        .get(&id)
        .cloned()
        .ok_or_else(|| FfiError::HostFailure(format!("{}: bad handle {}", who, id)))
}

// ---- Iterator registry -------------------------------------------------

// Eagerly-collected snapshot of key-value pairs for a scan or prefix scan.
// Collecting up-front avoids any lifetime coupling between the iterator and
// the DB, so the DB can be closed independently of the iterator handle.
struct IterState {
    // Remaining entries, stored front-to-back; we pop from the front via `pos`.
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    pos: usize,
}

struct IterRegistry {
    next_id: i64,
    slots: HashMap<i64, IterState>,
}

fn iter_registry() -> &'static Mutex<IterRegistry> {
    static R: OnceLock<Mutex<IterRegistry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(IterRegistry {
            next_id: 1,
            slots: HashMap::new(),
        })
    })
}

fn iter_lock() -> Result<std::sync::MutexGuard<'static, IterRegistry>, FfiError> {
    iter_registry()
        .lock()
        .map_err(|e| FfiError::HostFailure(format!("store: iter registry poisoned: {}", e)))
}

// ---- Argument helpers --------------------------------------------------

fn arity_err(name: &str, expected: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: expected.into(),
        got,
    }
}

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Fixnum(v)) => Ok(*v),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum",
            got: other.type_name().to_string(),
        }),
        None => Err(arity_err(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity_err(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_bv(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "bytevector",
            got: other.type_name().to_string(),
        }),
        None => Err(arity_err(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn opt_bool(args: &[Value], idx: usize) -> bool {
    match args.get(idx) {
        Some(Value::Boolean(b)) => *b,
        _ => false,
    }
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(Gc::new(std::cell::RefCell::new(b)))
}

fn cons_value(car: Value, cdr: Value) -> Value {
    Value::Pair(cs_core::Pair::new(car, cdr))
}

fn write_opts(sync: bool) -> WriteOptions {
    let mut opts = WriteOptions::default();
    opts.set_sync(sync);
    opts
}

// ---- Open DB with all existing CFs ------------------------------------

fn open_db(path: &str, create_if_missing: bool) -> Result<RocksDb, FfiError> {
    let mut opts = Options::default();
    opts.create_if_missing(create_if_missing);
    opts.create_missing_column_families(true);

    // Use plain DB::open (recovers existing data without listing CFs in Rust's
    // tracking map). DB::open_cf / open_cf_descriptors trigger a double-free in
    // RocksDB 10.4.2 when Rust-managed ColumnFamily handles are destroyed before
    // rocksdb_close(). The "default" CF is always implicitly available via the
    // non-CF put/get/delete/iterator methods. Non-default CFs created via
    // store-cf-create are tracked by create_cf() which adds them to the Rust map.
    RocksDb::open(&opts, path).map_err(|e| FfiError::HostFailure(format!("store-open: {}", e)))
}

// ---- Procedures --------------------------------------------------------

fn store_open(args: &[Value]) -> Result<Value, FfiError> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("store-open", "1 or 2", args.len()));
    }
    let path = expect_string("store-open", args, 0)?;
    // Default create_if_missing to true when only path is given (common case).
    let create = if args.len() == 1 {
        true
    } else {
        opt_bool(args, 1)
    };
    let db = open_db(&path, create)?;
    Ok(Value::fixnum(db_insert(db)?))
}

fn store_close(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity_err("store-close", "1", args.len()));
    }
    let id = expect_fixnum("store-close", args, 0)?;
    let mut r = db_lock()?;
    if r.slots.remove(&id).is_some() {
        Ok(Value::Unspecified)
    } else {
        Err(FfiError::HostFailure(format!(
            "store-close: handle {} not registered",
            id
        )))
    }
}

fn store_cf_create(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity_err("store-cf-create", "2", args.len()));
    }
    let id = expect_fixnum("store-cf-create", args, 0)?;
    let cf_name = expect_string("store-cf-create", args, 1)?;
    let db = db_get(id, "store-cf-create")?;
    db.create_cf(&cf_name, &Options::default())
        .map_err(|e| FfiError::HostFailure(format!("store-cf-create: {}", e)))?;
    Ok(Value::Unspecified)
}

fn store_get(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 3 {
        return Err(arity_err("store-get", "3", args.len()));
    }
    let id = expect_fixnum("store-get", args, 0)?;
    let cf_name = expect_string("store-get", args, 1)?;
    let key = expect_bv("store-get", args, 2)?;
    let db = db_get(id, "store-get")?;
    // For "default" use the non-CF method (DB was opened with DB::open which
    // does not register CFs in Rust's tracking map).
    let result = if cf_name == "default" {
        db.get(&key)
    } else {
        let cf = db
            .cf_handle(&cf_name)
            .ok_or_else(|| FfiError::HostFailure(format!("store-get: unknown CF {:?}", cf_name)))?;
        db.get_cf(&cf, &key)
    };
    match result.map_err(|e| FfiError::HostFailure(format!("store-get: {}", e)))? {
        Some(bytes) => Ok(bv_value(bytes)),
        None => Ok(Value::Boolean(false)),
    }
}

fn store_put(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 4 || args.len() > 5 {
        return Err(arity_err("store-put", "4 or 5", args.len()));
    }
    let id = expect_fixnum("store-put", args, 0)?;
    let cf_name = expect_string("store-put", args, 1)?;
    let key = expect_bv("store-put", args, 2)?;
    let val = expect_bv("store-put", args, 3)?;
    let sync = opt_bool(args, 4);
    let db = db_get(id, "store-put")?;
    if cf_name == "default" {
        db.put_opt(&key, &val, &write_opts(sync))
    } else {
        let cf = db
            .cf_handle(&cf_name)
            .ok_or_else(|| FfiError::HostFailure(format!("store-put: unknown CF {:?}", cf_name)))?;
        db.put_cf_opt(&cf, &key, &val, &write_opts(sync))
    }
    .map_err(|e| FfiError::HostFailure(format!("store-put: {}", e)))?;
    Ok(Value::Unspecified)
}

/// (store-put-many ID CF ((K . V) ...) [SYNC]) — cw-65x: write a whole
/// apply-batch as ONE RocksDB WriteBatch + one write. The hot consensus
/// apply path writes 2 rows per PUT; per-row store-put paid a WriteBatch
/// alloc + write + FFI round-trip each.
fn store_put_many(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(arity_err("store-put-many", "3 or 4", args.len()));
    }
    let id = expect_fixnum("store-put-many", args, 0)?;
    let cf_name = expect_string("store-put-many", args, 1)?;
    let sync = opt_bool(args, 3);
    let db = db_get(id, "store-put-many")?;
    let mut batch = WriteBatch::default();
    let cf = if cf_name == "default" {
        None
    } else {
        Some(db.cf_handle(&cf_name).ok_or_else(|| {
            FfiError::HostFailure(format!("store-put-many: unknown CF {:?}", cf_name))
        })?)
    };
    let mut cur = args[2].clone();
    let mut n: i64 = 0;
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                let (head, tail) = (p.car(), p.cdr());
                match &head {
                    Value::Pair(kv) => {
                        let k = match &kv.car() {
                            Value::ByteVector(b) => b.borrow().clone(),
                            v => {
                                return Err(FfiError::HostFailure(format!(
                                    "store-put-many: key not a bytevector: {:?}",
                                    v.type_name()
                                )))
                            }
                        };
                        let v = match &kv.cdr() {
                            Value::ByteVector(b) => b.borrow().clone(),
                            v => {
                                return Err(FfiError::HostFailure(format!(
                                    "store-put-many: value not a bytevector: {:?}",
                                    v.type_name()
                                )))
                            }
                        };
                        match &cf {
                            Some(cf) => batch.put_cf(cf, &k, &v),
                            None => batch.put(&k, &v),
                        }
                        n += 1;
                    }
                    _ => {
                        return Err(FfiError::HostFailure(
                            "store-put-many: entry not a (K . V) pair".into(),
                        ))
                    }
                }
                cur = tail;
            }
            _ => {
                return Err(FfiError::HostFailure(
                    "store-put-many: entries must be a proper list".into(),
                ))
            }
        }
    }
    db.write_opt(batch, &write_opts(sync))
        .map_err(|e| FfiError::HostFailure(format!("store-put-many: {}", e)))?;
    Ok(Value::Fixnum(n))
}

fn store_delete(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(arity_err("store-delete", "3 or 4", args.len()));
    }
    let id = expect_fixnum("store-delete", args, 0)?;
    let cf_name = expect_string("store-delete", args, 1)?;
    let key = expect_bv("store-delete", args, 2)?;
    let sync = opt_bool(args, 3);
    let db = db_get(id, "store-delete")?;
    if cf_name == "default" {
        db.delete_opt(&key, &write_opts(sync))
    } else {
        let cf = db.cf_handle(&cf_name).ok_or_else(|| {
            FfiError::HostFailure(format!("store-delete: unknown CF {:?}", cf_name))
        })?;
        db.delete_cf_opt(&cf, &key, &write_opts(sync))
    }
    .map_err(|e| FfiError::HostFailure(format!("store-delete: {}", e)))?;
    Ok(Value::Unspecified)
}

/// A single operation in a write batch.
/// Op tag is a **string** (`"put"` or `"delete"`) — no SymbolTable needed.
enum BatchOp {
    Put {
        cf: String,
        key: Vec<u8>,
        val: Vec<u8>,
    },
    Delete {
        cf: String,
        key: Vec<u8>,
    },
}

/// Walk a Scheme proper list into a `Vec<Value>`.
fn list_to_vec(v: &Value) -> Result<Vec<Value>, FfiError> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            _ => {
                return Err(FfiError::TypeMismatch {
                    expected: "proper list",
                    got: "improper list".to_string(),
                })
            }
        }
    }
}

fn parse_batch_op(v: &Value) -> Result<BatchOp, FfiError> {
    let elems = list_to_vec(v)?;

    let op_tag = match elems.first() {
        Some(Value::String(s)) => s.borrow().clone(),
        Some(other) => {
            return Err(FfiError::TypeMismatch {
                expected: "string (\"put\" or \"delete\")",
                got: other.type_name().to_string(),
            })
        }
        None => {
            return Err(FfiError::HostFailure(
                "store-write-batch: empty op".to_string(),
            ))
        }
    };

    match op_tag.as_str() {
        "put" => {
            if elems.len() != 4 {
                return Err(FfiError::HostFailure(format!(
                    "store-write-batch: put op requires 4 elements, got {}",
                    elems.len()
                )));
            }
            let cf = match &elems[1] {
                Value::String(s) => s.borrow().clone(),
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "string (CF name)",
                        got: other.type_name().to_string(),
                    })
                }
            };
            let key = match &elems[2] {
                Value::ByteVector(bv) => bv.borrow().clone(),
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "bytevector (key)",
                        got: other.type_name().to_string(),
                    })
                }
            };
            let val = match &elems[3] {
                Value::ByteVector(bv) => bv.borrow().clone(),
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "bytevector (value)",
                        got: other.type_name().to_string(),
                    })
                }
            };
            Ok(BatchOp::Put { cf, key, val })
        }
        "delete" => {
            if elems.len() != 3 {
                return Err(FfiError::HostFailure(format!(
                    "store-write-batch: delete op requires 3 elements, got {}",
                    elems.len()
                )));
            }
            let cf = match &elems[1] {
                Value::String(s) => s.borrow().clone(),
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "string (CF name)",
                        got: other.type_name().to_string(),
                    })
                }
            };
            let key = match &elems[2] {
                Value::ByteVector(bv) => bv.borrow().clone(),
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "bytevector (key)",
                        got: other.type_name().to_string(),
                    })
                }
            };
            Ok(BatchOp::Delete { cf, key })
        }
        other => Err(FfiError::HostFailure(format!(
            "store-write-batch: unknown op {:?} (expected \"put\" or \"delete\")",
            other
        ))),
    }
}

fn store_write_batch(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(arity_err("store-write-batch", "2 or 3", args.len()));
    }
    let id = expect_fixnum("store-write-batch", args, 0)?;
    let sync = opt_bool(args, 2);

    // Collect ops from the Scheme list (args[1]).
    let op_list = list_to_vec(&args[1])?;
    let mut ops: Vec<BatchOp> = Vec::with_capacity(op_list.len());
    for elem in &op_list {
        ops.push(parse_batch_op(elem)?);
    }

    let db = db_get(id, "store-write-batch")?;

    let mut batch = WriteBatch::default();
    for op in ops {
        match op {
            BatchOp::Put { cf, key, val } => {
                if cf == "default" {
                    batch.put(&key, &val);
                } else {
                    let cfh = db.cf_handle(&cf).ok_or_else(|| {
                        FfiError::HostFailure(format!("store-write-batch: unknown CF {:?}", cf))
                    })?;
                    batch.put_cf(&cfh, &key, &val);
                }
            }
            BatchOp::Delete { cf, key } => {
                if cf == "default" {
                    batch.delete(&key);
                } else {
                    let cfh = db.cf_handle(&cf).ok_or_else(|| {
                        FfiError::HostFailure(format!("store-write-batch: unknown CF {:?}", cf))
                    })?;
                    batch.delete_cf(&cfh, &key);
                }
            }
        }
    }

    db.write_opt(batch, &write_opts(sync))
        .map_err(|e| FfiError::HostFailure(format!("store-write-batch: {}", e)))?;
    Ok(Value::Unspecified)
}

fn store_iter(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 3 {
        return Err(arity_err("store-iter", "3", args.len()));
    }
    let id = expect_fixnum("store-iter", args, 0)?;
    let cf_name = expect_string("store-iter", args, 1)?;
    let prefix = expect_bv("store-iter", args, 2)?;

    // Collect all matching entries while holding the DB lock, then release it.
    // This avoids any lifetime coupling between the scan and the DB handle —
    // the DB can be closed independently of the iterator handle.
    let entries = {
        let db = db_get(id, "store-iter")?;
        let mut collected: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut raw = if cf_name == "default" {
            db.raw_iterator()
        } else {
            let cf = db.cf_handle(&cf_name).ok_or_else(|| {
                FfiError::HostFailure(format!("store-iter: unknown CF {:?}", cf_name))
            })?;
            db.raw_iterator_cf(&cf)
        };
        if prefix.is_empty() {
            raw.seek_to_first();
        } else {
            raw.seek(&prefix);
        }
        loop {
            if !raw.valid() {
                break;
            }
            let k = match raw.key() {
                Some(k) => k.to_vec(),
                None => break,
            };
            if !prefix.is_empty() && !k.starts_with(&prefix) {
                break;
            }
            let v = raw.value().unwrap_or(&[]).to_vec();
            collected.push((k, v));
            raw.next();
        }
        collected
    };

    let mut ir = iter_lock()?;
    let iter_id = ir.next_id;
    ir.next_id += 1;
    ir.slots.insert(iter_id, IterState { entries, pos: 0 });

    Ok(Value::fixnum(iter_id))
}

// store-iter-range: seek to START key and iterate (ascending) while key < END key.
// Unlike store-iter (prefix-bounded), this is a half-open RANGE [start, end) — the
// caller bounds the scan to exactly the rows it needs (e.g. a watch's (rev_lo, rev_hi]
// revision window), avoiding a full-namespace scan. (id cf start-bv end-bv) -> iter-id.
fn store_iter_range(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 4 {
        return Err(arity_err("store-iter-range", "4", args.len()));
    }
    let id = expect_fixnum("store-iter-range", args, 0)?;
    let cf_name = expect_string("store-iter-range", args, 1)?;
    let start = expect_bv("store-iter-range", args, 2)?;
    let end = expect_bv("store-iter-range", args, 3)?;
    let entries = {
        let db = db_get(id, "store-iter-range")?;
        let mut collected: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut raw = if cf_name == "default" {
            db.raw_iterator()
        } else {
            let cf = db.cf_handle(&cf_name).ok_or_else(|| {
                FfiError::HostFailure(format!("store-iter-range: unknown CF {:?}", cf_name))
            })?;
            db.raw_iterator_cf(&cf)
        };
        raw.seek(&start);
        loop {
            if !raw.valid() {
                break;
            }
            let k = match raw.key() {
                Some(k) => k.to_vec(),
                None => break,
            };
            if k.as_slice() >= end.as_slice() {
                break;
            }
            let v = raw.value().unwrap_or(&[]).to_vec();
            collected.push((k, v));
            raw.next();
        }
        collected
    };
    let mut ir = iter_lock()?;
    let iter_id = ir.next_id;
    ir.next_id += 1;
    ir.slots.insert(iter_id, IterState { entries, pos: 0 });
    Ok(Value::fixnum(iter_id))
}

// ---- store-range-latest-pb (cw-2au / LIST-scan wall) ----------------------
//
// The etcd-compatible KEY-CF "latest version per user key" range scan, fused
// scan+decode+protobuf-encode in one native pass. The interpreted per-row loop
// (Scheme scan -> tuple list -> cross-actor copy -> per-row pb encode) costs
// ~0.33ms/row, which caps a k8s cluster near 40k pods (a full LIST blows the
// apiserver's boot deadline). This walks RocksDB directly and returns the
// CONCATENATED field-2-tagged `repeated KeyValue kvs` submessages of an etcd
// RangeResponse, so Scheme splices ONE bytevector instead of touching rows.
//
// KEY-CF layout (crab-watchstore src/mvcc.scm, cw-zf7):
//   key   = 0x01 || esc(K) || 0x00 0x00 || INV(u64be(main) || u64be(sub))
//           esc: each 0x00 in K -> 0x00 0xFF; INV = per-byte complement
//           (versions sort newest-first within a key group)
//   value = tag(1: 0=value 1=tombstone) || cr(8) || mr(8) || ver(8) ||
//           lease(8) || vlen(8) || val    (all u64 big-endian)
//
// (store-range-latest-pb db cf start-fk end-fk rev limit keys-only? count-only?)
//   rev = 0: latest version per key; rev > 0: latest with main <= rev
//   -> (count more? pb-bytes)   count = live keys in range IGNORING limit;
//      more? = limit > 0 and count > emitted. Compaction floor checks stay in
//      Scheme (caller rejects reads below compact-rev before calling).
fn pb_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

fn pb_bytes_field(out: &mut Vec<u8>, field: u32, data: &[u8]) {
    pb_varint(out, ((field as u64) << 3) | 2);
    pb_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn pb_uint_field(out: &mut Vec<u8>, field: u32, v: u64) {
    if v != 0 {
        pb_varint(out, (field as u64) << 3);
        pb_varint(out, v);
    }
}

fn be64(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for &x in &b[..8] {
        v = (v << 8) | x as u64;
    }
    v
}

fn store_range_latest_pb(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 8 {
        return Err(arity_err("store-range-latest-pb", "8", args.len()));
    }
    let id = expect_fixnum("store-range-latest-pb", args, 0)?;
    let cf_name = expect_string("store-range-latest-pb", args, 1)?;
    let start = expect_bv("store-range-latest-pb", args, 2)?;
    let end = expect_bv("store-range-latest-pb", args, 3)?;
    let rev = expect_fixnum("store-range-latest-pb", args, 4)?;
    let limit = expect_fixnum("store-range-latest-pb", args, 5)?;
    let keys_only = opt_bool(args, 6);
    let count_only = opt_bool(args, 7);

    let db = db_get(id, "store-range-latest-pb")?;
    let mut raw = if cf_name == "default" {
        db.raw_iterator()
    } else {
        let cf = db.cf_handle(&cf_name).ok_or_else(|| {
            FfiError::HostFailure(format!("store-range-latest-pb: unknown CF {:?}", cf_name))
        })?;
        db.raw_iterator_cf(&cf)
    };

    // find the 0x00 0x00 terminator in a full KEY-CF key (0x00 0xFF = escaped
    // null mid-key). Returns byte offset of the terminator's first 0x00.
    fn find_term(fk: &[u8]) -> Option<usize> {
        let mut i = 1; // skip the NS byte
        while i + 1 < fk.len() {
            if fk[i] == 0 {
                if fk[i + 1] == 0 {
                    return Some(i);
                }
                i += 2; // escaped null
            } else {
                i += 1;
            }
        }
        None
    }

    let mut kvs: Vec<u8> = Vec::new();
    let mut count: i64 = 0;
    let mut emitted: i64 = 0;
    let mut user_key: Vec<u8> = Vec::new();
    let mut kv_buf: Vec<u8> = Vec::new();

    raw.seek(&start);
    while raw.valid() {
        let fk = match raw.key() {
            Some(k) if k < end.as_slice() => k,
            _ => break,
        };
        let term = match find_term(fk) {
            Some(t) if fk.len() >= t + 2 + 16 => t,
            _ => {
                raw.next();
                continue;
            }
        };
        // group prefix = everything through the terminator; owned so we can
        // advance the iterator while comparing against it.
        let prefix: Vec<u8> = fk[..term + 2].to_vec();

        // pick the candidate row: rev==0 -> first (newest); rev>0 -> first
        // with main <= rev (rows are newest-first).
        let mut candidate: Option<(u64, Vec<u8>)> = None; // (main, record)
        loop {
            let fk = match raw.key() {
                Some(k) if k < end.as_slice() && k.starts_with(&prefix) => k,
                _ => break,
            };
            let inv = &fk[term + 2..term + 2 + 8];
            let main = !be64(inv);
            if rev == 0 || main <= rev as u64 {
                candidate = Some((main, raw.value().unwrap_or(&[]).to_vec()));
                break;
            }
            raw.next();
        }

        if let Some((_, rec)) = candidate {
            // record: tag(1) cr(8) mr(8) ver(8) lease(8) vlen(8) val
            if rec.len() >= 41 && rec[0] == 0 {
                let cr = be64(&rec[1..9]);
                let mr = be64(&rec[9..17]);
                let ver = be64(&rec[17..25]);
                let lease = be64(&rec[25..33]);
                let vlen = be64(&rec[33..41]) as usize;
                count += 1;
                if !count_only && (limit == 0 || emitted < limit) {
                    // unescape the user key: 0x00 0xFF -> 0x00
                    user_key.clear();
                    let esc = &prefix[1..term];
                    let mut i = 0;
                    while i < esc.len() {
                        user_key.push(esc[i]);
                        if esc[i] == 0 {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    kv_buf.clear();
                    pb_bytes_field(&mut kv_buf, 1, &user_key);
                    pb_uint_field(&mut kv_buf, 2, cr);
                    pb_uint_field(&mut kv_buf, 3, mr);
                    pb_uint_field(&mut kv_buf, 4, ver);
                    if !keys_only && vlen > 0 && rec.len() >= 41 + vlen {
                        pb_bytes_field(&mut kv_buf, 5, &rec[41..41 + vlen]);
                    }
                    pb_uint_field(&mut kv_buf, 6, lease);
                    pb_bytes_field(&mut kvs, 2, &kv_buf); // RangeResponse.kvs
                    emitted += 1;
                }
            }
            // tombstone (rec[0]==1) or malformed: key is dead at this rev — skip.
        }

        // skip the rest of this key group; the outer `k < end` check ends the
        // whole scan if the next group starts past the range bound.
        while matches!(raw.key(), Some(k) if k.starts_with(&prefix)) {
            raw.next();
        }
    }

    let more = limit > 0 && count > emitted;
    Ok(cons_value(
        Value::fixnum(count),
        cons_value(Value::Boolean(more), cons_value(bv_value(kvs), Value::Null)),
    ))
}

fn store_iter_next(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity_err("store-iter-next", "1", args.len()));
    }
    let iter_id = expect_fixnum("store-iter-next", args, 0)?;
    let mut ir = iter_lock()?;
    let state = ir
        .slots
        .get_mut(&iter_id)
        .ok_or_else(|| FfiError::HostFailure(format!("store-iter-next: bad handle {}", iter_id)))?;

    if state.pos >= state.entries.len() {
        return Ok(Value::Boolean(false));
    }
    let (key, val) = state.entries[state.pos].clone();
    state.pos += 1;
    Ok(cons_value(bv_value(key), bv_value(val)))
}

fn store_iter_close(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity_err("store-iter-close", "1", args.len()));
    }
    let iter_id = expect_fixnum("store-iter-close", args, 0)?;
    let mut ir = iter_lock()?;
    if ir.slots.remove(&iter_id).is_some() {
        Ok(Value::Unspecified)
    } else {
        Err(FfiError::HostFailure(format!(
            "store-iter-close: handle {} not registered",
            iter_id
        )))
    }
}

/// `(store-seek handle cf seekkey prefix)` -> `(key . value)` | `#f`
///
/// One-shot O(log n) RocksDB Seek: position the iterator at the first key
/// `>= seekkey` and return that single `(key . value)` IFF the key still starts
/// with `prefix` (the bounding group), else `#f`. Unlike `store-iter` this does
/// NOT materialise the whole prefix range — it returns at most one row, which is
/// exactly what a "latest version <= readRev" MVCC point read needs (cw-u4a.38):
/// seek to `K || INV(rev16(readRev, MAX))` and take the first record still under
/// the `K` prefix.
fn store_seek(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 4 {
        return Err(arity_err("store-seek", "4", args.len()));
    }
    let id = expect_fixnum("store-seek", args, 0)?;
    let cf_name = expect_string("store-seek", args, 1)?;
    let seekkey = expect_bv("store-seek", args, 2)?;
    let prefix = expect_bv("store-seek", args, 3)?;

    let db = db_get(id, "store-seek")?;
    let mut raw = if cf_name == "default" {
        db.raw_iterator()
    } else {
        let cf = db.cf_handle(&cf_name).ok_or_else(|| {
            FfiError::HostFailure(format!("store-seek: unknown CF {:?}", cf_name))
        })?;
        db.raw_iterator_cf(&cf)
    };
    raw.seek(&seekkey);
    if !raw.valid() {
        return Ok(Value::Boolean(false));
    }
    let k = match raw.key() {
        Some(k) => k.to_vec(),
        None => return Ok(Value::Boolean(false)),
    };
    if !prefix.is_empty() && !k.starts_with(&prefix) {
        return Ok(Value::Boolean(false));
    }
    let v = raw.value().unwrap_or(&[]).to_vec();
    Ok(cons_value(bv_value(k), bv_value(v)))
}

fn store_checkpoint(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity_err("store-checkpoint", "2", args.len()));
    }
    let id = expect_fixnum("store-checkpoint", args, 0)?;
    let dir = expect_string("store-checkpoint", args, 1)?;
    let db = db_get(id, "store-checkpoint")?;
    let checkpoint = Checkpoint::new(&db)
        .map_err(|e| FfiError::HostFailure(format!("store-checkpoint: create: {}", e)))?;
    checkpoint
        .create_checkpoint(&dir)
        .map_err(|e| FfiError::HostFailure(format!("store-checkpoint: {}", e)))?;
    Ok(Value::Unspecified)
}

fn store_flush(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity_err("store-flush", "1", args.len()));
    }
    let id = expect_fixnum("store-flush", args, 0)?;
    let db = db_get(id, "store-flush")?;
    // DB was opened with DB::open so "default" CF is not in Rust's tracking
    // map — use the plain flush() which operates on the default CF internally.
    db.flush()
        .map_err(|e| FfiError::HostFailure(format!("store-flush: {}", e)))?;
    Ok(Value::Unspecified)
}

/// `store-flush-wal db [sync?]` — flush the WAL buffer to its file, fsyncing
/// it when `sync?` is true (default `#t`).
///
/// This is the group-commit primitive. With RocksDB's default settings
/// (`manual_wal_flush = false`), each `store-put`/`store-delete` issued with
/// `sync = #f` already flushes the WAL buffer to the OS but does NOT fsync.
/// A single `store-flush-wal db #t` then issues ONE fsync that durably
/// persists every such write accumulated since the last fsync — amortising
/// one disk barrier across many writers (cf. Redis group-commit AOF). Callers
/// MUST NOT ack a write as durable until this returns.
fn store_flush_wal(args: &[Value]) -> Result<Value, FfiError> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("store-flush-wal", "1 or 2", args.len()));
    }
    let id = expect_fixnum("store-flush-wal", args, 0)?;
    // Default to a syncing flush (the durable group-commit case). Pass #f only
    // to flush the WAL buffer to the OS without the fsync barrier.
    let sync = if args.len() == 2 {
        opt_bool(args, 1)
    } else {
        true
    };
    let db = db_get(id, "store-flush-wal")?;
    db.flush_wal(sync)
        .map_err(|e| FfiError::HostFailure(format!("store-flush-wal: {}", e)))?;
    Ok(Value::Unspecified)
}

// ---- Tests -------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Each test uses a unique temp path to avoid inter-test interference.
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_dir() -> String {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("cs-store-test-{}-{}", std::process::id(), n));
        path.to_string_lossy().to_string()
    }

    fn bv(b: &[u8]) -> Value {
        bv_value(b.to_vec())
    }

    fn str_val(s: &str) -> Value {
        Value::string(s.to_string())
    }

    fn fixnum(n: i64) -> Value {
        Value::fixnum(n)
    }

    fn scheme_list(items: &[Value]) -> Value {
        items
            .iter()
            .rev()
            .fold(Value::Null, |acc, v| cons_value(v.clone(), acc))
    }

    fn open(path: &str) -> Value {
        store_open(&[Value::string(path.to_string())]).unwrap()
    }

    fn as_fixnum(v: &Value) -> i64 {
        match v {
            Value::Fixnum(n) => *n,
            _ => panic!("expected fixnum, got {:?}", v),
        }
    }

    #[test]
    fn test_open_close() {
        let path = tmp_dir();
        let h = open(&path);
        let id = as_fixnum(&h);
        assert!(id > 0);
        store_close(&[h]).unwrap();
    }

    #[test]
    fn test_put_get() {
        let path = tmp_dir();
        let h = open(&path);
        let id = as_fixnum(&h);

        store_put(&[fixnum(id), str_val("default"), bv(b"hello"), bv(b"world")]).unwrap();

        let v = store_get(&[fixnum(id), str_val("default"), bv(b"hello")]).unwrap();
        assert!(
            matches!(&v, Value::ByteVector(bv) if bv.borrow().as_slice() == b"world"),
            "expected world, got {:?}",
            v
        );

        // Missing key returns #f
        let miss = store_get(&[fixnum(id), str_val("default"), bv(b"nope")]).unwrap();
        assert!(matches!(miss, Value::Boolean(false)));

        store_close(&[fixnum(id)]).unwrap();
    }

    #[test]
    fn test_delete() {
        let path = tmp_dir();
        let h = open(&path);
        let id = as_fixnum(&h);

        store_put(&[fixnum(id), str_val("default"), bv(b"k"), bv(b"v")]).unwrap();
        store_delete(&[fixnum(id), str_val("default"), bv(b"k")]).unwrap();

        let v = store_get(&[fixnum(id), str_val("default"), bv(b"k")]).unwrap();
        assert!(matches!(v, Value::Boolean(false)));

        store_close(&[fixnum(id)]).unwrap();
    }

    #[test]
    fn test_write_batch_atomic() {
        let path = tmp_dir();
        let h = open(&path);
        let id = as_fixnum(&h);

        // ops use string tags: (list "put" cf key val) / (list "delete" cf key)
        let op1 = scheme_list(&[str_val("put"), str_val("default"), bv(b"a"), bv(b"1")]);
        let op2 = scheme_list(&[str_val("put"), str_val("default"), bv(b"b"), bv(b"2")]);
        let ops = scheme_list(&[op1, op2]);

        store_write_batch(&[fixnum(id), ops]).unwrap();

        let va = store_get(&[fixnum(id), str_val("default"), bv(b"a")]).unwrap();
        let vb = store_get(&[fixnum(id), str_val("default"), bv(b"b")]).unwrap();
        assert!(matches!(&va, Value::ByteVector(bv) if bv.borrow().as_slice() == b"1"));
        assert!(matches!(&vb, Value::ByteVector(bv) if bv.borrow().as_slice() == b"2"));

        // delete op
        let del_op = scheme_list(&[str_val("delete"), str_val("default"), bv(b"a")]);
        let ops2 = scheme_list(&[del_op]);
        store_write_batch(&[fixnum(id), ops2]).unwrap();

        let va2 = store_get(&[fixnum(id), str_val("default"), bv(b"a")]).unwrap();
        assert!(matches!(va2, Value::Boolean(false)));

        store_close(&[fixnum(id)]).unwrap();
    }

    #[test]
    fn test_prefix_iter() {
        let path = tmp_dir();
        let h = open(&path);
        let id = as_fixnum(&h);

        store_put(&[fixnum(id), str_val("default"), bv(b"pfx:1"), bv(b"v1")]).unwrap();
        store_put(&[fixnum(id), str_val("default"), bv(b"pfx:2"), bv(b"v2")]).unwrap();
        store_put(&[fixnum(id), str_val("default"), bv(b"zzz"), bv(b"v3")]).unwrap();

        let iter_h = store_iter(&[fixnum(id), str_val("default"), bv(b"pfx:")]).unwrap();
        let iter_id = as_fixnum(&iter_h);

        let r1 = store_iter_next(&[fixnum(iter_id)]).unwrap();
        assert!(
            matches!(&r1, Value::Pair(_)),
            "expected pair r1, got {:?}",
            r1
        );

        let r2 = store_iter_next(&[fixnum(iter_id)]).unwrap();
        assert!(
            matches!(&r2, Value::Pair(_)),
            "expected pair r2, got {:?}",
            r2
        );

        // Third call should be #f (prefix exhausted)
        let r3 = store_iter_next(&[fixnum(iter_id)]).unwrap();
        assert!(matches!(r3, Value::Boolean(false)));

        store_iter_close(&[fixnum(iter_id)]).unwrap();
        store_close(&[fixnum(id)]).unwrap();
    }

    #[test]
    fn test_checkpoint() {
        let path = tmp_dir();
        let ckpt_path = format!("{}-ckpt", path);
        // RocksDB creates <ckpt_path>.tmp before renaming; clean both to avoid
        // C++ assert() if they exist from a previous interrupted run.
        let _ = std::fs::remove_dir_all(format!("{}.tmp", ckpt_path));
        let _ = std::fs::remove_dir_all(&ckpt_path);
        let h = open(&path);
        let id = as_fixnum(&h);

        store_put(&[fixnum(id), str_val("default"), bv(b"ck"), bv(b"val")]).unwrap();
        store_checkpoint(&[fixnum(id), Value::string(ckpt_path.clone())]).unwrap();

        assert!(
            std::path::Path::new(&ckpt_path).exists(),
            "checkpoint dir missing"
        );

        store_close(&[fixnum(id)]).unwrap();
    }

    #[test]
    fn test_reopen_and_read() {
        let path = tmp_dir();

        {
            let h = open(&path);
            let id = as_fixnum(&h);
            store_put(&[fixnum(id), str_val("default"), bv(b"persist"), bv(b"yes")]).unwrap();
            store_close(&[fixnum(id)]).unwrap();
        }

        {
            let h = open(&path);
            let id = as_fixnum(&h);
            let v = store_get(&[fixnum(id), str_val("default"), bv(b"persist")]).unwrap();
            assert!(
                matches!(&v, Value::ByteVector(bv) if bv.borrow().as_slice() == b"yes"),
                "expected 'yes' after reopen"
            );
            store_close(&[fixnum(id)]).unwrap();
        }
    }

    #[test]
    fn test_flush_wal_persists_unsynced_writes() {
        // Group-commit invariant: writes made with sync=#f are durable once
        // store-flush-wal(sync=#t) returns. Simulate by writing unsynced,
        // flushing the WAL, dropping the handle (no explicit close = closest
        // we can get to a crash in-process without kill -9), then reopening.
        let path = tmp_dir();
        {
            let h = open(&path);
            let id = as_fixnum(&h);
            // sync = #f on every write (the group-commit batched-write regime)
            store_put(&[
                fixnum(id),
                str_val("default"),
                bv(b"g1"),
                bv(b"v1"),
                Value::Boolean(false),
            ])
            .unwrap();
            store_put(&[
                fixnum(id),
                str_val("default"),
                bv(b"g2"),
                bv(b"v2"),
                Value::Boolean(false),
            ])
            .unwrap();
            // single fsync covering both writes
            store_flush_wal(&[fixnum(id), Value::Boolean(true)]).unwrap();
            store_close(&[fixnum(id)]).unwrap();
        }
        {
            let h = open(&path);
            let id = as_fixnum(&h);
            let v1 = store_get(&[fixnum(id), str_val("default"), bv(b"g1")]).unwrap();
            let v2 = store_get(&[fixnum(id), str_val("default"), bv(b"g2")]).unwrap();
            assert!(
                matches!(&v1, Value::ByteVector(b) if b.borrow().as_slice() == b"v1"),
                "g1 lost after flush_wal+reopen"
            );
            assert!(
                matches!(&v2, Value::ByteVector(b) if b.borrow().as_slice() == b"v2"),
                "g2 lost after flush_wal+reopen"
            );
            store_close(&[fixnum(id)]).unwrap();
        }
    }

    #[test]
    fn test_cf_create() {
        let path = tmp_dir();
        let h = open(&path);
        let id = as_fixnum(&h);

        store_cf_create(&[fixnum(id), str_val("mycf")]).unwrap();
        store_put(&[fixnum(id), str_val("mycf"), bv(b"k"), bv(b"v")]).unwrap();
        let v = store_get(&[fixnum(id), str_val("mycf"), bv(b"k")]).unwrap();
        assert!(matches!(&v, Value::ByteVector(bv) if bv.borrow().as_slice() == b"v"));

        store_close(&[fixnum(id)]).unwrap();
    }

    #[test]
    fn test_flush() {
        let path = tmp_dir();
        let h = open(&path);
        let id = as_fixnum(&h);
        store_put(&[fixnum(id), str_val("default"), bv(b"x"), bv(b"y")]).unwrap();
        store_flush(&[fixnum(id)]).unwrap();
        store_close(&[fixnum(id)]).unwrap();
    }
}
