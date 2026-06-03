//! CrabScheme stdlib module: `(crab hash)`.
//!
//! Cryptographic hashes (SHA-1/256/512, MD5, BLAKE3) and HMAC-SHA256.
//! Iter 7 of the `stdlib-modules` spec. Supersedes the example
//! `cs-ffi-sha2` crate (which stays as the FFI-plugin teaching
//! example for third-party authors).
//!
//! Each hash procedure accepts either a bytevector or a string;
//! strings are hashed as their UTF-8 byte sequence. Returns a
//! bytevector. Pair with `(crab base)` for hex/base64 rendering.
//!
//! Streaming `(hash-create algo)` / `(hash-update! …)` /
//! `(hash-finalize …)` need an opaque-payload Scheme value; tracked
//! for a follow-up iter alongside `Value::Opaque`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `hash-sha256`  | string or bv     | bytevector | 32 bytes |
//! | `hash-sha512`  | string or bv     | bytevector | 64 bytes |
//! | `hash-sha1`    | string or bv     | bytevector | 20 bytes (legacy; not for new security uses) |
//! | `hash-md5`     | string or bv     | bytevector | 16 bytes (legacy; checksum/etag use only) |
//! | `hash-blake3`  | string or bv     | bytevector | 32 bytes |
//! | `hmac-sha256`  | key bv str-or-bv | bytevector | 32 bytes |

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("hash-sha256", hash_sha256),
        UntypedProc::new("hash-sha512", hash_sha512),
        UntypedProc::new("hash-sha1", hash_sha1),
        UntypedProc::new("hash-md5", hash_md5),
        UntypedProc::new("hash-blake3", hash_blake3),
        UntypedProc::new("hmac-sha256", hmac_sha256),
        UntypedProc::new("crc32", crc32),
        UntypedProc::new("adler32", adler32),
        UntypedProc::new("fnv1a-32", fnv1a_32),
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

/// Accept a Scheme string or bytevector; return its raw bytes.
fn expect_bytes(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().as_bytes().to_vec()),
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string or bytevector",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

// ----- procedures -----

fn hash_sha256(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("hash-sha256", args, 0)?;
    let mut h = Sha256::new();
    h.update(&input);
    Ok(bv_value(h.finalize().to_vec()))
}

fn hash_sha512(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("hash-sha512", args, 0)?;
    let mut h = Sha512::new();
    h.update(&input);
    Ok(bv_value(h.finalize().to_vec()))
}

fn hash_sha1(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("hash-sha1", args, 0)?;
    let mut h = Sha1::new();
    h.update(&input);
    Ok(bv_value(h.finalize().to_vec()))
}

fn hash_md5(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("hash-md5", args, 0)?;
    let mut h = Md5::new();
    h.update(&input);
    Ok(bv_value(h.finalize().to_vec()))
}

fn hash_blake3(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("hash-blake3", args, 0)?;
    Ok(bv_value(blake3::hash(&input).as_bytes().to_vec()))
}

fn hmac_sha256(args: &[Value]) -> Result<Value, FfiError> {
    let key = expect_bytes("hmac-sha256", args, 0)?;
    let msg = expect_bytes("hmac-sha256", args, 1)?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key)
        .map_err(|e| FfiError::HostFailure(format!("hmac-sha256: key length {}", e)))?;
    mac.update(&msg);
    Ok(bv_value(mac.finalize().into_bytes().to_vec()))
}

// ----- non-cryptographic checksums (return a fixnum, not a bytevector) -----

/// `(crc32 data)` — CRC-32 (IEEE, the zip/png/gzip polynomial).
fn crc32(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("crc32", args, 0)?;
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in &input {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    Ok(Value::fixnum((!crc) as i64))
}

/// `(adler32 data)` — Adler-32 (the zlib checksum).
fn adler32(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("adler32", args, 0)?;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &x in &input {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    Ok(Value::fixnum((((b << 16) | a) as i64) & 0xFFFF_FFFF))
}

/// `(fnv1a-32 data)` — 32-bit FNV-1a hash (fast non-cryptographic hash).
fn fnv1a_32(args: &[Value]) -> Result<Value, FfiError> {
    let input = expect_bytes("fnv1a-32", args, 0)?;
    let mut h: u32 = 0x811c_9dc5;
    for &b in &input {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    Ok(Value::fixnum(h as i64))
}
