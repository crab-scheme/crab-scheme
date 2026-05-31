//! CrabScheme stdlib module: `(crab sql)`.
//!
//! Embedded SQLite — the `(crab …)` answer to Python's `sqlite3`,
//! Go's `database/sql`, and Clojure's JDBC wrappers. Backed by
//! `rusqlite` with the bundled SQLite amalgamation, so there's no
//! external database to install. (Bundling vendors C, so this module
//! is native-only — it is not part of `wasm-stdlib`.)
//!
//! A connection is an opaque handle (`#('__sql-conn__ id)`) stored in
//! a per-thread registry; the CrabScheme runtime is single-threaded.
//! Close a connection with `sql-close!` to release it; handles are
//! otherwise held until the program exits.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `sql-open`            | path                | connection | `":memory:"` for an in-memory DB. |
//! | `sql-close!`          | conn                | unspec     | Idempotent. |
//! | `sql-connection?`     | value               | boolean    | Handle predicate. |
//! | `sql-execute`         | conn sql . params   | fixnum     | Rows changed (INSERT/UPDATE/DELETE/DDL). |
//! | `sql-execute-batch`   | conn sql-script     | unspec     | Run a multi-statement script (no params). |
//! | `sql-query`           | conn sql . params   | list       | Rows, each an alist of (column-name . value). |
//! | `sql-query-row`       | conn sql . params   | alist or #f | First row, or `#f` if none. |
//! | `sql-query-value`     | conn sql . params   | value or #f | First column of first row, or `#f`. |
//! | `sql-last-insert-id`  | conn                | fixnum     | Rowid of the last insert. |
//!
//! ## Value ↔ SQLite type mapping
//!
//! | Scheme (param) | SQLite | SQLite (result) | Scheme |
//! |---|---|---|---|
//! | exact integer  | INTEGER | INTEGER | fixnum |
//! | flonum / rational | REAL | REAL  | flonum |
//! | string         | TEXT    | TEXT   | string |
//! | bytevector     | BLOB    | BLOB   | bytevector |
//! | `#t` / `#f`     | INTEGER 1 / 0 | NULL | `#f` |
//! | `'()` / unspecified | NULL | | |
//!
//! SQL `NULL` reads back as `#f`. Exact integers larger than 2^53 in
//! magnitude lose exactness when bound as parameters (they pass
//! through an `f64`); results always come back as full-range fixnums.
//!
//! Transactions work through `sql-execute`: run `"BEGIN"`, then
//! `"COMMIT"` or `"ROLLBACK"`.
//!
//! ```scheme
//! (import (crab sql))
//! (define db (sql-open ":memory:"))
//! (sql-execute-batch db "create table todo(id integer primary key, what text);")
//! (sql-execute db "insert into todo(what) values (?)" "buy milk")
//! (sql-query db "select * from todo")     ; => ((("id" . 1) ("what" . "buy milk")))
//! (sql-query-value db "select count(*) from todo")   ; => 1
//! (sql-close! db)
//! ```

use std::cell::RefCell;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::Connection;

const CONN_TAG: &str = "__sql-conn__";
/// Largest magnitude an `f64` represents with full integer precision.
const MAX_EXACT_F64_INT: f64 = 9_007_199_254_740_992.0; // 2^53

thread_local! {
    /// Per-thread connection registry. The Scheme handle stores an
    /// index into this vector; `sql-close!` tomb-stones the slot to
    /// `None`, dropping the `Connection` (which closes the database).
    static CONNS: RefCell<Vec<Option<Connection>>> = const { RefCell::new(Vec::new()) };
}

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("sql-open", sql_open),
        UntypedProc::new("sql-close!", sql_close),
        UntypedProc::new("sql-connection?", sql_connection_p),
        UntypedProc::new("sql-execute", sql_execute),
        UntypedProc::new("sql-execute-batch", sql_execute_batch),
        UntypedProc::new("sql-query", sql_query),
        UntypedProc::new("sql-query-row", sql_query_row),
        UntypedProc::new("sql-query-value", sql_query_value),
        UntypedProc::new("sql-last-insert-id", sql_last_insert_id),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn fail(msg: String) -> FfiError {
    FfiError::HostFailure(msg)
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(RefCell::new(b)))
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

// ----- connection registry -----

fn register(c: Connection) -> u32 {
    CONNS.with(|slot| {
        let mut v = slot.borrow_mut();
        let id = v.len() as u32;
        v.push(Some(c));
        id
    })
}

fn conn_value(id: u32) -> Value {
    Value::Vector(cs_core::Gc::new(RefCell::new(vec![
        Value::string(CONN_TAG),
        Value::fixnum(id as i64),
    ])))
}

fn is_conn_value(v: &Value) -> bool {
    if let Value::Vector(items) = v {
        let items = items.borrow();
        items.len() == 2 && matches!(&items[0], Value::String(s) if s.borrow().as_str() == CONN_TAG)
    } else {
        false
    }
}

fn decode_id(v: &Value) -> Option<u32> {
    let Value::Vector(items) = v else {
        return None;
    };
    let items = items.borrow();
    if items.len() != 2 {
        return None;
    }
    if !matches!(&items[0], Value::String(s) if s.borrow().as_str() == CONN_TAG) {
        return None;
    }
    match &items[1] {
        Value::Number(n) => u32::try_from(n.to_f64() as i64).ok(),
        _ => None,
    }
}

/// Run `f` against the open `Connection` named by handle `conn`.
fn with_conn<R>(
    name: &str,
    conn: &Value,
    f: impl FnOnce(&Connection) -> rusqlite::Result<R>,
) -> Result<R, FfiError> {
    let id = decode_id(conn).ok_or_else(|| FfiError::TypeMismatch {
        expected: "sql connection",
        got: conn.type_name().to_string(),
    })?;
    CONNS.with(|slot| {
        let v = slot.borrow();
        let inst = v
            .get(id as usize)
            .and_then(|o| o.as_ref())
            .ok_or_else(|| fail(format!("{}: connection is closed or invalid", name)))?;
        f(inst).map_err(|e| fail(format!("{}: {}", name, e)))
    })
}

// ----- value conversion -----

/// Scheme value → SQLite bind parameter.
fn to_sql(name: &str, v: &Value) -> Result<SqlValue, FfiError> {
    Ok(match v {
        Value::Null | Value::Unspecified => SqlValue::Null,
        Value::Boolean(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => {
            let f = n.to_f64();
            if n.is_exact() && n.is_integer() && f.is_finite() && f.abs() < MAX_EXACT_F64_INT {
                SqlValue::Integer(f as i64)
            } else {
                SqlValue::Real(f)
            }
        }
        Value::String(s) => SqlValue::Text(s.borrow().clone()),
        Value::ByteVector(bv) => SqlValue::Blob(bv.borrow().clone()),
        other => {
            return Err(FfiError::TypeMismatch {
                expected: "number, string, bytevector, boolean, or null (sql parameter)",
                got: format!("{} (in {})", other.type_name(), name),
            })
        }
    })
}

/// SQLite result cell → Scheme value. SQL NULL maps to `#f`.
fn valueref_to_value(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Boolean(false),
        ValueRef::Integer(i) => Value::fixnum(i),
        ValueRef::Real(f) => Value::flonum(f),
        ValueRef::Text(b) => Value::string(String::from_utf8_lossy(b).into_owned()),
        ValueRef::Blob(b) => bv_value(b.to_vec()),
    }
}

/// Collect bind parameters from `args[from..]`.
fn collect_params(name: &str, args: &[Value], from: usize) -> Result<Vec<SqlValue>, FfiError> {
    args[from..].iter().map(|v| to_sql(name, v)).collect()
}

/// Run a SELECT and return every row as a `Vec` of (column, value).
fn run_query(
    name: &str,
    conn: &Value,
    sql: &str,
    params: &[SqlValue],
) -> Result<Vec<Vec<(String, Value)>>, FfiError> {
    with_conn(name, conn, |c| {
        let mut stmt = c.prepare(sql)?;
        let ncols = stmt.column_count();
        let names: Vec<String> = (0..ncols)
            .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
            .collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let mut out: Vec<Vec<(String, Value)>> = Vec::new();
        while let Some(row) = rows.next()? {
            let mut entry = Vec::with_capacity(ncols);
            for (i, col) in names.iter().enumerate() {
                entry.push((col.clone(), valueref_to_value(row.get_ref(i)?)));
            }
            out.push(entry);
        }
        Ok(out)
    })
}

fn row_to_alist(entry: Vec<(String, Value)>) -> Value {
    Value::list(
        entry
            .into_iter()
            .map(|(k, v)| Value::Pair(cs_core::Pair::new(Value::string(k), v))),
    )
}

// ----- procedures -----

fn sql_open(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("sql-open", "1", args.len()));
    }
    let path = expect_string("sql-open", args, 0)?;
    let conn = Connection::open(&path).map_err(|e| fail(format!("sql-open: {}: {}", path, e)))?;
    Ok(conn_value(register(conn)))
}

fn sql_close(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("sql-close!", "1", args.len()));
    }
    let id = decode_id(&args[0]).ok_or_else(|| FfiError::TypeMismatch {
        expected: "sql connection",
        got: args[0].type_name().to_string(),
    })?;
    // Tomb-stone the slot, dropping the Connection. Idempotent: closing
    // an already-closed (or out-of-range) handle is a no-op.
    CONNS.with(|slot| {
        if let Some(opt) = slot.borrow_mut().get_mut(id as usize) {
            *opt = None;
        }
    });
    Ok(Value::Unspecified)
}

fn sql_connection_p(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("sql-connection?", "1", args.len()));
    }
    Ok(Value::Boolean(is_conn_value(&args[0])))
}

fn sql_execute(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 2 {
        return Err(arity("sql-execute", ">= 2", args.len()));
    }
    let sql = expect_string("sql-execute", args, 1)?;
    let params = collect_params("sql-execute", args, 2)?;
    let changed = with_conn("sql-execute", &args[0], |c| {
        c.execute(&sql, rusqlite::params_from_iter(params.iter()))
    })?;
    Ok(Value::fixnum(changed as i64))
}

fn sql_execute_batch(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("sql-execute-batch", "2", args.len()));
    }
    let sql = expect_string("sql-execute-batch", args, 1)?;
    with_conn("sql-execute-batch", &args[0], |c| c.execute_batch(&sql))?;
    Ok(Value::Unspecified)
}

fn sql_query(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 2 {
        return Err(arity("sql-query", ">= 2", args.len()));
    }
    let sql = expect_string("sql-query", args, 1)?;
    let params = collect_params("sql-query", args, 2)?;
    let rows = run_query("sql-query", &args[0], &sql, &params)?;
    Ok(Value::list(rows.into_iter().map(row_to_alist)))
}

fn sql_query_row(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 2 {
        return Err(arity("sql-query-row", ">= 2", args.len()));
    }
    let sql = expect_string("sql-query-row", args, 1)?;
    let params = collect_params("sql-query-row", args, 2)?;
    let rows = run_query("sql-query-row", &args[0], &sql, &params)?;
    Ok(rows
        .into_iter()
        .next()
        .map_or(Value::Boolean(false), row_to_alist))
}

fn sql_query_value(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 2 {
        return Err(arity("sql-query-value", ">= 2", args.len()));
    }
    let sql = expect_string("sql-query-value", args, 1)?;
    let params = collect_params("sql-query-value", args, 2)?;
    let rows = run_query("sql-query-value", &args[0], &sql, &params)?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|mut row| {
            if row.is_empty() {
                None
            } else {
                Some(row.remove(0).1)
            }
        })
        .unwrap_or(Value::Boolean(false)))
}

fn sql_last_insert_id(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("sql-last-insert-id", "1", args.len()));
    }
    let id = with_conn(
        "sql-last-insert-id",
        &args[0],
        |c| Ok(c.last_insert_rowid()),
    )?;
    Ok(Value::fixnum(id))
}
