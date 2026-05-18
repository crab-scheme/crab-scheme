//! CrabScheme stdlib module: `(crab archive)`.
//!
//! Tar and Zip archive operations. Iter 7 of the `stdlib-modules`
//! spec.
//!
//! Surface in this iter is intentionally narrow: list the contents
//! and extract to disk. Programmatic creation (`tar-create`,
//! `zip-create`) and per-entry streaming need a richer Value model
//! and land in a follow-up iter alongside `Value::Opaque`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `tar-list`         | archive-path           | list of strings | Entry paths. |
//! | `tar-extract`      | archive-path dest-dir  | unspec          | Files written under dest-dir. |
//! | `tar-gz-list`      | archive-path           | list of strings | gzip-wrapped tar (.tar.gz, .tgz). |
//! | `tar-gz-extract`   | archive-path dest-dir  | unspec          |
//! | `zip-list`         | archive-path           | list of strings |
//! | `zip-extract`      | archive-path dest-dir  | unspec          |

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use flate2::read::GzDecoder;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("tar-list", tar_list),
        UntypedProc::new("tar-extract", tar_extract),
        UntypedProc::new("tar-gz-list", tar_gz_list),
        UntypedProc::new("tar-gz-extract", tar_gz_extract),
        UntypedProc::new("zip-list", zip_list),
        UntypedProc::new("zip-extract", zip_extract),
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

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn io_fail(name: &str, path: &str, e: std::io::Error) -> FfiError {
    FfiError::HostFailure(format!("{}: {}: {}", name, path, e))
}

// ----- tar -----

fn tar_list_inner<R: std::io::Read>(name: &str, reader: R) -> Result<Value, FfiError> {
    let mut archive = tar::Archive::new(reader);
    let mut names = Vec::new();
    for entry in archive
        .entries()
        .map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?
    {
        let entry = entry.map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?;
        let path = entry
            .path()
            .map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?;
        names.push(string_value(path.to_string_lossy().into_owned()));
    }
    Ok(Value::list(names))
}

fn tar_extract_inner<R: std::io::Read>(
    name: &str,
    reader: R,
    dest: &str,
) -> Result<Value, FfiError> {
    let mut archive = tar::Archive::new(reader);
    archive
        .unpack(Path::new(dest))
        .map_err(|e| io_fail(name, dest, e))?;
    Ok(Value::Unspecified)
}

fn tar_list(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("tar-list", args, 0)?;
    let f = File::open(&path).map_err(|e| io_fail("tar-list", &path, e))?;
    tar_list_inner("tar-list", f)
}

fn tar_extract(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("tar-extract", args, 0)?;
    let dest = expect_string("tar-extract", args, 1)?;
    let f = File::open(&path).map_err(|e| io_fail("tar-extract", &path, e))?;
    tar_extract_inner("tar-extract", f, &dest)
}

fn tar_gz_list(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("tar-gz-list", args, 0)?;
    let f = File::open(&path).map_err(|e| io_fail("tar-gz-list", &path, e))?;
    tar_list_inner("tar-gz-list", GzDecoder::new(f))
}

fn tar_gz_extract(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("tar-gz-extract", args, 0)?;
    let dest = expect_string("tar-gz-extract", args, 1)?;
    let f = File::open(&path).map_err(|e| io_fail("tar-gz-extract", &path, e))?;
    tar_extract_inner("tar-gz-extract", GzDecoder::new(f), &dest)
}

// ----- zip -----

fn zip_list(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("zip-list", args, 0)?;
    let f = File::open(&path).map_err(|e| io_fail("zip-list", &path, e))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| FfiError::HostFailure(format!("zip-list: {}: {}", path, e)))?;
    let mut names = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| FfiError::HostFailure(format!("zip-list: entry {}: {}", i, e)))?;
        names.push(string_value(entry.name().to_string()));
    }
    Ok(Value::list(names))
}

fn zip_extract(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("zip-extract", args, 0)?;
    let dest = expect_string("zip-extract", args, 1)?;
    let f = File::open(&path).map_err(|e| io_fail("zip-extract", &path, e))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| FfiError::HostFailure(format!("zip-extract: {}: {}", path, e)))?;
    zip.extract(Path::new(&dest))
        .map_err(|e| FfiError::HostFailure(format!("zip-extract: {}: {}", dest, e)))?;
    Ok(Value::Unspecified)
}
