//! CrabScheme stdlib module: `(crab uuid)`.
//!
//! UUID v4 (random) and v7 (timestamp + random) generation, parsing,
//! and round-tripping. Iter 5 of the `stdlib-modules` spec.
//!
//! UUIDs are passed around as 36-char strings (`8-4-4-4-12` hex) to
//! avoid the opaque-payload problem; a typed `uuid?` predicate +
//! handle lands when `Value::Opaque` does.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `uuid-v4`        | —                  | string  | Random UUIDv4. |
//! | `uuid-v7`        | —                  | string  | Timestamp + random UUIDv7. |
//! | `uuid-valid?`    | string             | boolean | True iff `uuid-parse` would succeed. |
//! | `uuid-version`   | string             | fixnum or #f | UUID version number, or #f when unparseable. |

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use uuid::Uuid;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("uuid-v4", uuid_v4),
        UntypedProc::new("uuid-v7", uuid_v7),
        UntypedProc::new("uuid-valid?", uuid_valid_p),
        UntypedProc::new("uuid-version", uuid_version),
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
            expected: "string".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

// ----- procedures -----

fn uuid_v4(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("uuid-v4", "0", args.len()));
    }
    Ok(string_value(Uuid::new_v4().to_string()))
}

fn uuid_v7(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("uuid-v7", "0", args.len()));
    }
    Ok(string_value(Uuid::now_v7().to_string()))
}

fn uuid_valid_p(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("uuid-valid?", args, 0)?;
    Ok(Value::Boolean(Uuid::parse_str(&s).is_ok()))
}

fn uuid_version(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("uuid-version", args, 0)?;
    match Uuid::parse_str(&s) {
        Ok(u) => Ok(Value::fixnum(u.get_version_num() as i64)),
        Err(_) => Ok(Value::Boolean(false)),
    }
}
