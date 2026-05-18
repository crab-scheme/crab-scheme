//! CrabScheme stdlib module: `(crab compress)`.
//!
//! gzip / deflate / zstd compression via `flate2` and `zstd`.
//! Iter 7 of the `stdlib-modules` spec.
//!
//! All procedures slurp the full input into memory and produce a
//! single output bytevector. Streaming variants (compressor as a
//! Scheme port wrapping a writer) need an opaque-payload Scheme
//! value; tracked for a follow-up iter.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `gzip-compress`      | bv [level] | bytevector | level 0–9; default 6 |
//! | `gzip-decompress`    | bv         | bytevector |
//! | `deflate-compress`   | bv [level] | bytevector | raw deflate (no gzip header) |
//! | `deflate-decompress` | bv         | bytevector |
//! | `zstd-compress`      | bv [level] | bytevector | level 1–22; default 3 |
//! | `zstd-decompress`    | bv         | bytevector |

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
        UntypedProc::new("zstd-compress", zstd_compress),
        UntypedProc::new("zstd-decompress", zstd_decompress),
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
        Some(Value::Number(n)) => {
            let v = n.to_f64() as i64;
            if v < 0 || (v as u32) > max {
                Err(FfiError::HostFailure(format!(
                    "compress level must be 0..={}; got {}",
                    max, v
                )))
            } else {
                Ok(v as u32)
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

// ----- gzip -----

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
    let mut out = Vec::new();
    GzDecoder::new(&input[..])
        .read_to_end(&mut out)
        .map_err(|e| FfiError::HostFailure(format!("gzip-decompress: {}", e)))?;
    Ok(bv_value(out))
}

// ----- deflate -----

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
    let mut out = Vec::new();
    DeflateDecoder::new(&input[..])
        .read_to_end(&mut out)
        .map_err(|e| FfiError::HostFailure(format!("deflate-decompress: {}", e)))?;
    Ok(bv_value(out))
}

// ----- zstd -----

fn zstd_compress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("zstd-compress", args, 0)?;
    let level = opt_level(args, 1, 3, 22)? as i32;
    zstd::encode_all(std::io::Cursor::new(input), level)
        .map(bv_value)
        .map_err(|e| FfiError::HostFailure(format!("zstd-compress: {}", e)))
}

fn zstd_decompress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("zstd-decompress", args, 0)?;
    zstd::decode_all(std::io::Cursor::new(input))
        .map(bv_value)
        .map_err(|e| FfiError::HostFailure(format!("zstd-decompress: {}", e)))
}
