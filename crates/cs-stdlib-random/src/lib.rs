//! CrabScheme stdlib module: `(crab random)`.
//!
//! Random number generation backed by the `rand` crate. Iter 5
//! of the `stdlib-modules` spec.
//!
//! Sources are not yet exposed (`make-random-source` /
//! `random-source-state-ref` / `…-set!`) — those need an
//! opaque-payload Scheme value. This iter exposes the thread-local
//! default RNG (`rand::thread_rng()`), which is cryptographically
//! seeded from the OS per Rust's `ThreadRng` contract.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `random-bytes`     | fixnum             | bytevector | Cryptographic; from OS entropy via `ThreadRng`. |
//! | `random-integer`   | fixnum             | fixnum  | In `[0, n)`. Errors if n ≤ 0. |
//! | `random-flonum`    | —                  | flonum  | In `[0.0, 1.0)`. |
//! | `random-choice`    | non-empty list     | value   | Uniform sample. |

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use rand::{thread_rng, Rng, RngCore};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("random-bytes", random_bytes),
        UntypedProc::new("random-integer", random_integer),
        UntypedProc::new("random-flonum", random_flonum),
        UntypedProc::new("random-choice", random_choice),
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

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Number(n)) => Ok(n.to_f64() as i64),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

// ----- procedures -----

fn random_bytes(args: &[Value]) -> Result<Value, FfiError> {
    let n = expect_fixnum("random-bytes", args, 0)?;
    if n < 0 {
        return Err(FfiError::HostFailure(
            "random-bytes: negative length".into(),
        ));
    }
    let mut buf = vec![0u8; n as usize];
    thread_rng().fill_bytes(&mut buf);
    Ok(Value::ByteVector(cs_core::Gc::new(
        std::cell::RefCell::new(buf),
    )))
}

fn random_integer(args: &[Value]) -> Result<Value, FfiError> {
    let n = expect_fixnum("random-integer", args, 0)?;
    if n <= 0 {
        return Err(FfiError::HostFailure(
            "random-integer: upper bound must be positive".into(),
        ));
    }
    Ok(Value::fixnum(thread_rng().gen_range(0..n)))
}

fn random_flonum(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("random-flonum", "0", args.len()));
    }
    Ok(Value::flonum(thread_rng().r#gen::<f64>()))
}

fn random_choice(args: &[Value]) -> Result<Value, FfiError> {
    let mut cur = args
        .first()
        .cloned()
        .ok_or(arity("random-choice", ">= 1", 0))?;
    let mut items: Vec<Value> = Vec::new();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                items.push(p.car());
                cur = p.cdr();
            }
            other => {
                return Err(FfiError::TypeMismatch {
                    expected: "proper list".into(),
                    got: other.type_name().to_string(),
                });
            }
        }
    }
    if items.is_empty() {
        return Err(FfiError::HostFailure("random-choice: empty list".into()));
    }
    let idx = thread_rng().gen_range(0..items.len());
    Ok(items.swap_remove(idx))
}
