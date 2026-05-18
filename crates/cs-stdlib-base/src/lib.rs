//! CrabScheme stdlib module: `(crab base)`.
//!
//! Base-N encoding / decoding (base64 standard, base64 URL-safe,
//! hex). Iter 6 of the `stdlib-modules` spec.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `base64-encode`     | bytevector | string     | Standard alphabet, with `=` padding. |
//! | `base64-decode`     | string     | bytevector | Errors on invalid input. |
//! | `base64url-encode`  | bytevector | string     | URL-safe alphabet, no padding. |
//! | `base64url-decode`  | string     | bytevector |
//! | `hex-encode`        | bytevector | string     | Lowercase. |
//! | `hex-decode`        | string     | bytevector | Accepts upper or lower case. |

use std::sync::Arc;

use base64::engine::general_purpose;
use base64::Engine;
use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("base64-encode", base64_encode),
        UntypedProc::new("base64-decode", base64_decode),
        UntypedProc::new("base64url-encode", base64url_encode),
        UntypedProc::new("base64url-decode", base64url_decode),
        UntypedProc::new("hex-encode", hex_encode),
        UntypedProc::new("hex-decode", hex_decode),
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
            expected: "bytevector".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

// ----- base64 -----

fn base64_encode(args: &[Value]) -> Result<Value, FfiError> {
    let b = expect_bv("base64-encode", args, 0)?;
    Ok(string_value(general_purpose::STANDARD.encode(&b)))
}

fn base64_decode(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("base64-decode", args, 0)?;
    general_purpose::STANDARD
        .decode(s.as_bytes())
        .map(bv_value)
        .map_err(|e| FfiError::HostFailure(format!("base64-decode: {}", e)))
}

fn base64url_encode(args: &[Value]) -> Result<Value, FfiError> {
    let b = expect_bv("base64url-encode", args, 0)?;
    Ok(string_value(general_purpose::URL_SAFE_NO_PAD.encode(&b)))
}

fn base64url_decode(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("base64url-decode", args, 0)?;
    general_purpose::URL_SAFE_NO_PAD
        .decode(s.as_bytes())
        .map(bv_value)
        .map_err(|e| FfiError::HostFailure(format!("base64url-decode: {}", e)))
}

// ----- hex -----

fn hex_encode(args: &[Value]) -> Result<Value, FfiError> {
    let b = expect_bv("hex-encode", args, 0)?;
    Ok(string_value(hex::encode(&b)))
}

fn hex_decode(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("hex-decode", args, 0)?;
    hex::decode(&s)
        .map(bv_value)
        .map_err(|e| FfiError::HostFailure(format!("hex-decode: {}", e)))
}
