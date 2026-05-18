//! CrabScheme stdlib module: `(crab fs)`.
//!
//! Filesystem operations on top of `std::fs`. Iter 2 of the
//! `stdlib-modules` spec.
//!
//! Errors return as `FfiError::HostFailure(msg)` for now — the
//! `&stdlib-fs` condition hierarchy from design.md FR-6 lands in
//! a follow-up iter once `cs-core` exposes a condition constructor
//! the FFI layer can call.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `read-file-string`     | string         | string     | UTF-8 only; non-UTF-8 raises an error. |
//! | `read-file-bytes`      | string         | bytevector | Raw bytes; works for binary files. |
//! | `write-file-string`    | string string  | unspec     | Replaces existing content. |
//! | `write-file-bytes`     | string bvec    | unspec     | Replaces existing content. |
//! | `append-file-string`   | string string  | unspec     | Appends; creates if missing. |
//! | `append-file-bytes`    | string bvec    | unspec     | Appends; creates if missing. |
//! | `delete-file`          | string         | unspec     | No-op if file missing? No — raises. |
//! | `rename-file`          | string string  | unspec     | Atomic when src/dst same filesystem. |
//! | `copy-file`            | string string  | unspec     | Overwrites dst. |
//! | `file-exists?`         | string         | boolean    | False for non-existent, true for files. |
//! | `directory-exists?`    | string         | boolean    | True only for directories. |
//! | `directory-list`       | string         | list of strings | Names only (not full paths). |
//! | `directory-create`     | string         | unspec     | One level; errors if parent missing. |
//! | `directory-create-all` | string         | unspec     | mkdir -p semantics. |
//! | `directory-delete`     | string         | unspec     | Errors if non-empty. |
//! | `file-size`            | string         | fixnum     | Bytes. |

use std::fs;
use std::path::Path;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

/// Every `(crab fs)` procedure as a Vec of `HostProcedure`
/// factories. cs-runtime iterates this and calls
/// `register_host_procedure` per entry.
pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("read-file-string", read_file_string),
        UntypedProc::new("read-file-bytes", read_file_bytes),
        UntypedProc::new("write-file-string", write_file_string),
        UntypedProc::new("write-file-bytes", write_file_bytes),
        UntypedProc::new("append-file-string", append_file_string),
        UntypedProc::new("append-file-bytes", append_file_bytes),
        UntypedProc::new("delete-file", delete_file),
        UntypedProc::new("rename-file", rename_file),
        UntypedProc::new("copy-file", copy_file),
        UntypedProc::new("file-exists?", file_exists_p),
        UntypedProc::new("directory-exists?", directory_exists_p),
        UntypedProc::new("directory-list", directory_list),
        UntypedProc::new("directory-create", directory_create),
        UntypedProc::new("directory-create-all", directory_create_all),
        UntypedProc::new("directory-delete", directory_delete),
        UntypedProc::new("file-size", file_size),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: usize, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.to_string(),
        got,
    }
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, idx + 1, args.len())),
    }
}

fn expect_bytevector(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "bytevector".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, idx + 1, args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn bytevector_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

fn io_fail(name: &str, path: &str, e: std::io::Error) -> FfiError {
    FfiError::HostFailure(format!("{}: {}: {}", name, path, e))
}

// ----- read -----

fn read_file_string(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("read-file-string", args, 0)?;
    match fs::read_to_string(&p) {
        Ok(s) => Ok(string_value(s)),
        Err(e) => Err(io_fail("read-file-string", &p, e)),
    }
}

fn read_file_bytes(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("read-file-bytes", args, 0)?;
    match fs::read(&p) {
        Ok(b) => Ok(bytevector_value(b)),
        Err(e) => Err(io_fail("read-file-bytes", &p, e)),
    }
}

// ----- write -----

fn write_file_string(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("write-file-string", args, 0)?;
    let s = expect_string("write-file-string", args, 1)?;
    match fs::write(&p, s.as_bytes()) {
        Ok(()) => Ok(Value::Unspecified),
        Err(e) => Err(io_fail("write-file-string", &p, e)),
    }
}

fn write_file_bytes(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("write-file-bytes", args, 0)?;
    let b = expect_bytevector("write-file-bytes", args, 1)?;
    match fs::write(&p, &b) {
        Ok(()) => Ok(Value::Unspecified),
        Err(e) => Err(io_fail("write-file-bytes", &p, e)),
    }
}

fn append_file_string(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("append-file-string", args, 0)?;
    let s = expect_string("append-file-string", args, 1)?;
    append_bytes("append-file-string", &p, s.as_bytes())
}

fn append_file_bytes(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("append-file-bytes", args, 0)?;
    let b = expect_bytevector("append-file-bytes", args, 1)?;
    append_bytes("append-file-bytes", &p, &b)
}

fn append_bytes(name: &str, path: &str, data: &[u8]) -> Result<Value, FfiError> {
    use std::io::Write;
    let mut f = match fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(e) => return Err(io_fail(name, path, e)),
    };
    match f.write_all(data) {
        Ok(()) => Ok(Value::Unspecified),
        Err(e) => Err(io_fail(name, path, e)),
    }
}

// ----- file ops -----

fn delete_file(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("delete-file", args, 0)?;
    fs::remove_file(&p)
        .map(|_| Value::Unspecified)
        .map_err(|e| io_fail("delete-file", &p, e))
}

fn rename_file(args: &[Value]) -> Result<Value, FfiError> {
    let src = expect_string("rename-file", args, 0)?;
    let dst = expect_string("rename-file", args, 1)?;
    fs::rename(&src, &dst)
        .map(|_| Value::Unspecified)
        .map_err(|e| io_fail("rename-file", &src, e))
}

fn copy_file(args: &[Value]) -> Result<Value, FfiError> {
    let src = expect_string("copy-file", args, 0)?;
    let dst = expect_string("copy-file", args, 1)?;
    fs::copy(&src, &dst)
        .map(|_| Value::Unspecified)
        .map_err(|e| io_fail("copy-file", &src, e))
}

fn file_exists_p(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("file-exists?", args, 0)?;
    Ok(Value::Boolean(Path::new(&p).is_file()))
}

fn file_size(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("file-size", args, 0)?;
    fs::metadata(&p)
        .map(|m| Value::fixnum(m.len() as i64))
        .map_err(|e| io_fail("file-size", &p, e))
}

// ----- directory ops -----

fn directory_exists_p(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("directory-exists?", args, 0)?;
    Ok(Value::Boolean(Path::new(&p).is_dir()))
}

fn directory_list(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("directory-list", args, 0)?;
    let entries = match fs::read_dir(&p) {
        Ok(e) => e,
        Err(e) => return Err(io_fail("directory-list", &p, e)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => return Err(io_fail("directory-list", &p, e)),
        };
        names.push(string_value(
            entry.file_name().to_string_lossy().into_owned(),
        ));
    }
    Ok(Value::list(names))
}

fn directory_create(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("directory-create", args, 0)?;
    fs::create_dir(&p)
        .map(|_| Value::Unspecified)
        .map_err(|e| io_fail("directory-create", &p, e))
}

fn directory_create_all(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("directory-create-all", args, 0)?;
    fs::create_dir_all(&p)
        .map(|_| Value::Unspecified)
        .map_err(|e| io_fail("directory-create-all", &p, e))
}

fn directory_delete(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("directory-delete", args, 0)?;
    fs::remove_dir(&p)
        .map(|_| Value::Unspecified)
        .map_err(|e| io_fail("directory-delete", &p, e))
}
