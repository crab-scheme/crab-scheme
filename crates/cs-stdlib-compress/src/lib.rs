//! CrabScheme stdlib module: `(crab compress)` — zstd only.
//!
//! Iter 17 splits gzip + raw deflate into `cs-stdlib-deflate`
//! so the WASM build can ship those without inheriting
//! `zstd-sys`'s C-toolchain requirements (the nix-wrapped clang
//! in our build env rejects flags zstd-sys' build script passes
//! for `wasm32-wasip1`).
//!
//! ## Registered procedures
//!
//! Decompression takes an optional `max-output-bytes` argument
//! (default: 64 MB) as a decompression-bomb mitigation —
//! caller-controlled compressed input can otherwise expand 1 KB
//! → several GB with no warning. Pass a larger cap explicitly
//! when processing trusted bulk data.
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `zstd-compress`      | bv [level] | bytevector | level 1–22; default 3 |
//! | `zstd-decompress`    | bv [max]   | bytevector | max default 64 MB |

use std::io::Read;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("zstd-compress", zstd_compress),
        UntypedProc::new("zstd-decompress", zstd_decompress),
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
        Some(Value::Fixnum(v)) => {
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

#[cfg(not(target_arch = "wasm32"))]
fn zstd_compress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("zstd-compress", args, 0)?;
    let level = opt_level(args, 1, 3, 22)? as i32;
    zstd::encode_all(std::io::Cursor::new(input), level)
        .map(bv_value)
        .map_err(|e| FfiError::HostFailure(format!("zstd-compress: {}", e)))
}

#[cfg(not(target_arch = "wasm32"))]
fn zstd_decompress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("zstd-decompress", args, 0)?;
    let max = opt_max_bytes(args, 1)?;
    let decoder = zstd::Decoder::new(std::io::Cursor::new(input))
        .map_err(|e| FfiError::HostFailure(format!("zstd-decompress: {}", e)))?;
    let out = read_capped("zstd-decompress", decoder, max)?;
    Ok(bv_value(out))
}

// WASM uses `ruzstd` — pure-Rust port of zstd. ruzstd 0.8 only
// implements the `Uncompressed` and `Fastest` encoder levels
// (Default/Better/Best panic with `unimplemented!()`), so we map
// every requested level to `Fastest`. Output is still valid zstd
// — native consumers decode it fine; only the compression ratio
// is weaker than what the C zstd would give at the same level.
// Decompression supports the full format.
#[cfg(target_arch = "wasm32")]
fn zstd_compress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("zstd-compress", args, 0)?;
    // Still parse the level for argument-validation parity with
    // the native path; we don't actually use the value.
    let _level = opt_level(args, 1, 3, 22)?;
    let out =
        ruzstd::encoding::compress_to_vec(&input[..], ruzstd::encoding::CompressionLevel::Fastest);
    Ok(bv_value(out))
}

#[cfg(target_arch = "wasm32")]
fn zstd_decompress(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bv("zstd-decompress", args, 0)?;
    let max = opt_max_bytes(args, 1)?;
    let decoder = ruzstd::decoding::StreamingDecoder::new(&input[..])
        .map_err(|e| FfiError::HostFailure(format!("zstd-decompress: {}", e)))?;
    let out = read_capped("zstd-decompress", decoder, max)?;
    Ok(bv_value(out))
}
