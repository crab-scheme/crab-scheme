//! CrabScheme stdlib module: `(crab deflate)`.
//!
//! gzip + raw deflate compression via `flate2`. Split out of
//! `cs-stdlib-compress` in iter 17 so the WASM build can ship
//! gzip/deflate without inheriting `zstd-sys`'s C-toolchain
//! requirements (the nix-wrapped clang in our build env rejects
//! flags zstd-sys' build script passes for `wasm32-wasip1`).
//!
//! Procedure names are unchanged from the pre-split surface — a
//! caller that does `(import (crab compress))` and writes
//! `(gzip-compress …)` keeps working as long as both
//! `stdlib-deflate` and `stdlib-compress` are enabled (default).
//! WASM subset embedders enable only `stdlib-deflate`.
//!
//! Decompression-bomb mitigation: `gzip-decompress` and
//! `deflate-decompress` take an optional `max-output-bytes`
//! argument (default 64 MB) and raise rather than allocate
//! unbounded when input expands past the cap.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `gzip-compress`      | bv [level] | bytevector | level 0–9; default 6 |
//! | `gzip-decompress`    | bv [max]   | bytevector | max default 64 MB |
//! | `deflate-compress`   | bv [level] | bytevector | raw deflate (no gzip header) |
//! | `deflate-decompress` | bv [max]   | bytevector | max default 64 MB |

use std::io::{Read, Write};
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::{DeflateEncoder, GzEncoder};
use flate2::Compression;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("gzip-compress", gzip_compress),
        UntypedProc::new("gzip-decompress", gzip_decompress),
        UntypedProc::new("deflate-compress", deflate_compress),
        UntypedProc::new("deflate-decompress", deflate_decompress),
    ]
}

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn expect_bv(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "bytevector",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn opt_level(args: &[Value], idx: usize, default: u32, max: u32) -> Result<u32, FfiError> {
    match args.get(idx) {
        None => Ok(default),
        Some(Value::Number(cs_core::Number::Fixnum(v))) => {
            if *v < 0 || (*v as u32) > max {
                Err(FfiError::HostFailure(format!(
                    "compress level must be 0..={}; got {}",
                    max, v
                )))
            } else {
                Ok(*v as u32)
            }
        }
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum or no arg",
            got: other.type_name().to_string(),
        }),
    }
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

const DEFAULT_MAX_DECOMPRESSED: u64 = 64 * 1024 * 1024;

fn opt_max_bytes(args: &[Value], idx: usize) -> Result<u64, FfiError> {
    match args.get(idx) {
        None => Ok(DEFAULT_MAX_DECOMPRESSED),
        Some(Value::Number(cs_core::Number::Fixnum(v))) if *v >= 0 => Ok(*v as u64),
        Some(Value::Number(cs_core::Number::Fixnum(v))) => Err(FfiError::HostFailure(format!(
            "max-output-bytes must be non-negative; got {}",
            v
        ))),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum or no arg",
            got: other.type_name().to_string(),
        }),
    }
}

fn read_capped<R: Read>(name: &str, mut r: R, max_bytes: u64) -> Result<Vec<u8>, FfiError> {
    let mut out = Vec::new();
    let n = (&mut r)
        .take(max_bytes + 1)
        .read_to_end(&mut out)
        .map_err(|e| FfiError::HostFailure(format!("{}: {}", name, e)))?;
    if (n as u64) > max_bytes {
        return Err(FfiError::HostFailure(format!(
            "{}: decompressed output exceeds {} bytes (decompression-bomb guard); pass a larger max-output-bytes if expected",
            name, max_bytes
        )));
    }
    Ok(out)
}

fn gzip_compress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("gzip-compress", args, 0)?;
    let level = opt_level(args, 1, 6, 9)?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(&input)
        .map_err(|e| FfiError::HostFailure(format!("gzip-compress: {}", e)))?;
    Ok(bv_value(enc.finish().map_err(|e| {
        FfiError::HostFailure(format!("gzip-compress: {}", e))
    })?))
}

fn gzip_decompress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("gzip-decompress", args, 0)?;
    let max = opt_max_bytes(args, 1)?;
    let out = read_capped("gzip-decompress", GzDecoder::new(&input[..]), max)?;
    Ok(bv_value(out))
}

fn deflate_compress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("deflate-compress", args, 0)?;
    let level = opt_level(args, 1, 6, 9)?;
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(&input)
        .map_err(|e| FfiError::HostFailure(format!("deflate-compress: {}", e)))?;
    Ok(bv_value(enc.finish().map_err(|e| {
        FfiError::HostFailure(format!("deflate-compress: {}", e))
    })?))
}

fn deflate_decompress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("deflate-decompress", args, 0)?;
    let max = opt_max_bytes(args, 1)?;
    let out = read_capped("deflate-decompress", DeflateDecoder::new(&input[..]), max)?;
    Ok(bv_value(out))
}
