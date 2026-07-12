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
//! ## Security model — extraction
//!
//! `tar::Archive::unpack` is documented as "best effort" against
//! symlink-then-write attacks (an entry creates a symlink under
//! `dest`, then a later entry writes through it to a path outside
//! `dest`). We do NOT call `unpack`. Instead, every entry is
//! validated manually:
//!
//! - Entry path must canonicalize to a location *inside* `dest`
//!   (component-wise check; rejects `..`, absolute paths, and
//!   post-symlink writes).
//! - Symlink and hardlink entries are **rejected outright** — no
//!   way for an archive to install a symlink that subsequent
//!   entries (or the user) could traverse.
//! - Existing files at the destination are **not overwritten**
//!   (a hostile archive can't replace `~/.bashrc`).
//! - Total bytes written is capped (default 256 MB) to guard
//!   against decompression bombs in `.tar.gz`.
//!
//! `zip` 2.x's `ZipArchive::extract` already strips `..` and
//! absolute paths, but we still apply manual entry-walking for
//! defense-in-depth: rejects symlink-mode bits and caps bytes
//! written.
//!
//! Callers processing trusted archives can pass a larger
//! `max-output-bytes` arg if 256 MB is too tight.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `tar-list`         | archive-path                | list of strings | Entry paths. |
//! | `tar-extract`      | archive-path dest-dir [max] | unspec          | Files written under dest-dir; max default 256 MB. |
//! | `tar-gz-list`      | archive-path                | list of strings | gzip-wrapped tar (.tar.gz, .tgz). |
//! | `tar-gz-extract`   | archive-path dest-dir [max] | unspec          | max default 256 MB. |
//! | `zip-list`         | archive-path                | list of strings |
//! | `zip-extract`      | archive-path dest-dir [max] | unspec          | max default 256 MB. |
//! | `zip-create`       | archive-path entries        | unspec          | entries: list of (name . string-or-bytevector); deflate. |

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use flate2::read::GzDecoder;

// Cap on bytes written to disk during extraction. Defense against
// `.tar.gz` decompression bombs and oversized archives. 256 MB is
// large enough for typical source-code archives, language-server
// distributions, etc. Callers handling trusted bulk data can pass
// a larger explicit limit.
const DEFAULT_MAX_EXTRACTED: u64 = 256 * 1024 * 1024;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("tar-list", tar_list),
        UntypedProc::new("tar-extract", tar_extract),
        UntypedProc::new("tar-gz-list", tar_gz_list),
        UntypedProc::new("tar-gz-extract", tar_gz_extract),
        UntypedProc::new("zip-list", zip_list),
        UntypedProc::new("zip-extract", zip_extract),
        UntypedProc::new("zip-create", zip_create),
    ]
}

/// `(zip-create archive-path entries)` — write a zip (deflate) from a list
/// of `(name . content)` pairs, where content is a string or bytevector.
fn zip_create(args: &[Value]) -> Result<Value, FfiError> {
    let name = "zip-create";
    let path = expect_string(name, args, 0)?;
    let entries = args
        .get(1)
        .cloned()
        .ok_or_else(|| arity(name, "2", args.len()))?;
    let f = File::create(&path).map_err(|e| io_fail(name, &path, e))?;
    let mut zip = zip::ZipWriter::new(f);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut cur = entries;
    while let Value::Pair(p) = cur {
        let (ename, content) = match p.car() {
            Value::Pair(ep) => (ep.car(), ep.cdr()),
            other => {
                return Err(FfiError::HostFailure(format!(
                    "{}: each entry must be a (name . content) pair, got {}",
                    name,
                    other.type_name()
                )))
            }
        };
        let ename = match ename {
            Value::String(s) => s.borrow().clone(),
            other => {
                return Err(FfiError::TypeMismatch {
                    expected: "string (entry name)",
                    got: other.type_name().to_string(),
                })
            }
        };
        let bytes: Vec<u8> = match content {
            Value::String(s) => s.borrow().as_bytes().to_vec(),
            Value::ByteVector(b) => b.borrow().clone(),
            other => {
                return Err(FfiError::TypeMismatch {
                    expected: "string or bytevector (entry content)",
                    got: other.type_name().to_string(),
                })
            }
        };
        zip.start_file(ename, options)
            .map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?;
        zip.write_all(&bytes).map_err(|e| io_fail(name, &path, e))?;
        cur = p.cdr();
    }
    zip.finish()
        .map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?;
    Ok(Value::Unspecified)
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
    Value::string(s)
}

fn io_fail(name: &str, path: &str, e: std::io::Error) -> FfiError {
    FfiError::HostFailure(format!("{}: {}: {}", name, path, e))
}

fn opt_max_bytes(args: &[Value], idx: usize) -> Result<u64, FfiError> {
    match args.get(idx) {
        None => Ok(DEFAULT_MAX_EXTRACTED),
        Some(Value::Fixnum(v)) if *v >= 0 => Ok(*v as u64),
        Some(Value::Fixnum(v)) => Err(FfiError::HostFailure(format!(
            "max-output-bytes must be non-negative; got {}",
            v
        ))),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum or no arg",
            got: other.type_name().to_string(),
        }),
    }
}

// Build a safe destination path by joining `dest` with each
// component of `entry`. Rejects absolute paths, `..`, root, and
// device-prefix components. Returns the joined `PathBuf` or an
// error suitable for surfacing to Scheme.
fn safe_join(name: &str, dest: &Path, entry: &Path) -> Result<PathBuf, FfiError> {
    let mut out = dest.to_path_buf();
    for c in entry.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(FfiError::HostFailure(format!(
                    "{}: archive entry path escapes dest: {}",
                    name,
                    entry.display()
                )));
            }
        }
    }
    Ok(out)
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
    max_bytes: u64,
) -> Result<Value, FfiError> {
    let dest_path = Path::new(dest);
    std::fs::create_dir_all(dest_path).map_err(|e| io_fail(name, dest, e))?;

    let mut archive = tar::Archive::new(reader);
    let mut bytes_written: u64 = 0;

    let entries = archive
        .entries()
        .map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?;

    for entry in entries {
        let mut entry = entry.map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?;

        // Reject symlinks and hardlinks outright. A symlink entry can
        // point anywhere on the filesystem, and a subsequent file entry
        // writing through it escapes `dest`. We have no use case for
        // symlinked archive contents that justifies the attack surface.
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(FfiError::HostFailure(format!(
                "{}: archive contains symlink/hardlink entry (rejected as unsafe)",
                name
            )));
        }

        let entry_path = entry
            .path()
            .map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?
            .into_owned();
        let target = safe_join(name, dest_path, &entry_path)?;

        if kind.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| io_fail(name, &target.display().to_string(), e))?;
            continue;
        }
        if !kind.is_file() {
            // Skip device/fifo/char entries silently — they aren't
            // meaningful inside a portable extraction.
            continue;
        }

        // Size check BEFORE write so an oversized archive can't fill
        // the disk halfway through.
        let entry_size = entry.header().size().unwrap_or(0);
        if bytes_written.saturating_add(entry_size) > max_bytes {
            return Err(FfiError::HostFailure(format!(
                "{}: extracted output exceeds {} bytes (archive-bomb guard); pass a larger max-output-bytes if expected",
                name, max_bytes
            )));
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_fail(name, &parent.display().to_string(), e))?;
        }
        // create_new(true) refuses to overwrite existing files. A
        // hostile archive can't replace something on disk that wasn't
        // there before extraction.
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| io_fail(name, &target.display().to_string(), e))?;
        let copied = std::io::copy(&mut entry, &mut out)
            .map_err(|e| io_fail(name, &target.display().to_string(), e))?;
        bytes_written = bytes_written.saturating_add(copied);
        if bytes_written > max_bytes {
            return Err(FfiError::HostFailure(format!(
                "{}: extracted output exceeds {} bytes (archive-bomb guard)",
                name, max_bytes
            )));
        }
    }
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
    let max = opt_max_bytes(args, 2)?;
    let f = File::open(&path).map_err(|e| io_fail("tar-extract", &path, e))?;
    tar_extract_inner("tar-extract", f, &dest, max)
}

fn tar_gz_list(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("tar-gz-list", args, 0)?;
    let f = File::open(&path).map_err(|e| io_fail("tar-gz-list", &path, e))?;
    tar_list_inner("tar-gz-list", GzDecoder::new(f))
}

fn tar_gz_extract(args: &[Value]) -> Result<Value, FfiError> {
    let path = expect_string("tar-gz-extract", args, 0)?;
    let dest = expect_string("tar-gz-extract", args, 1)?;
    let max = opt_max_bytes(args, 2)?;
    let f = File::open(&path).map_err(|e| io_fail("tar-gz-extract", &path, e))?;
    tar_extract_inner("tar-gz-extract", GzDecoder::new(f), &dest, max)
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
    let name = "zip-extract";
    let path = expect_string(name, args, 0)?;
    let dest = expect_string(name, args, 1)?;
    let max = opt_max_bytes(args, 2)?;
    let dest_path = Path::new(&dest);
    std::fs::create_dir_all(dest_path).map_err(|e| io_fail(name, &dest, e))?;

    let f = File::open(&path).map_err(|e| io_fail(name, &path, e))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| FfiError::HostFailure(format!("{}: {}: {}", name, path, e)))?;
    let mut bytes_written: u64 = 0;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| FfiError::HostFailure(format!("{}: entry {}: {}", name, i, e)))?;

        // `enclosed_name` returns None for entries with `..` or
        // absolute paths — refuse those outright rather than silently
        // dropping the entry the way `ZipArchive::extract` does.
        let entry_path = entry.enclosed_name().ok_or_else(|| {
            FfiError::HostFailure(format!(
                "{}: entry {} has unsafe path: {}",
                name,
                i,
                entry.name()
            ))
        })?;
        let target = safe_join(name, dest_path, &entry_path)?;

        // Refuse symlink-mode entries (defense-in-depth — the default
        // `zip` feature set doesn't create symlinks but a future
        // feature flip could).
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            const S_IFLNK: u32 = 0o120000;
            if mode & 0o170000 == S_IFLNK {
                return Err(FfiError::HostFailure(format!(
                    "{}: entry {} is a symlink (rejected as unsafe)",
                    name, i
                )));
            }
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| io_fail(name, &target.display().to_string(), e))?;
            continue;
        }

        let entry_size = entry.size();
        if bytes_written.saturating_add(entry_size) > max {
            return Err(FfiError::HostFailure(format!(
                "{}: extracted output exceeds {} bytes (archive-bomb guard); pass a larger max-output-bytes if expected",
                name, max
            )));
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_fail(name, &parent.display().to_string(), e))?;
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| io_fail(name, &target.display().to_string(), e))?;
        let copied = std::io::copy(&mut entry, &mut out)
            .map_err(|e| io_fail(name, &target.display().to_string(), e))?;
        bytes_written = bytes_written.saturating_add(copied);
        if bytes_written > max {
            return Err(FfiError::HostFailure(format!(
                "{}: extracted output exceeds {} bytes (archive-bomb guard)",
                name, max
            )));
        }
    }
    Ok(Value::Unspecified)
}
