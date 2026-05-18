//! CrabScheme stdlib module: `(crab time)`.
//!
//! Wall-clock and monotonic time, sleep, strftime-style formatting.
//! Iter 5 of the `stdlib-modules` spec.
//!
//! Until the FFI layer gains an opaque-payload Scheme value, time
//! values are represented as fixnums (epoch seconds for wall, nanos
//! since process start for monotonic) instead of typed Instant /
//! Duration handles. `format-time` and `parse-time` work in epoch
//! seconds; `time-add` / `time-diff` are trivially `+` / `-` of two
//! fixnums and aren't bound as builtins for that reason.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `current-time`       | —                  | fixnum  | Unix epoch seconds. |
//! | `current-time-ms`    | —                  | fixnum  | Unix epoch milliseconds. |
//! | `monotonic-time-ns`  | —                  | fixnum  | Nanoseconds since process start. Strictly increases. |
//! | `sleep-ms`           | fixnum             | unspec  | Block current thread N ms. |
//! | `format-time`        | fixnum string      | string  | strftime; arg 1 is epoch seconds. |
//! | `parse-time`         | string string      | fixnum or #f | strptime; returns epoch seconds. |

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("current-time", current_time),
        UntypedProc::new("current-time-ms", current_time_ms),
        UntypedProc::new("monotonic-time-ns", monotonic_time_ns),
        UntypedProc::new("sleep-ms", sleep_ms),
        UntypedProc::new("format-time", format_time),
        UntypedProc::new("parse-time", parse_time),
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

// Process-start instant; lazily initialized on first call. Subsequent
// calls measure elapsed nanos against this baseline so the value
// strictly increases across the process lifetime.
fn process_start() -> Instant {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

// ----- wall clock -----

fn current_time(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("current-time", "0", args.len()));
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FfiError::HostFailure(format!("current-time: {}", e)))?
        .as_secs() as i64;
    Ok(Value::fixnum(secs))
}

fn current_time_ms(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("current-time-ms", "0", args.len()));
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FfiError::HostFailure(format!("current-time-ms: {}", e)))?
        .as_millis() as i64;
    Ok(Value::fixnum(ms))
}

// ----- monotonic -----

fn monotonic_time_ns(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("monotonic-time-ns", "0", args.len()));
    }
    let ns = process_start().elapsed().as_nanos() as i64;
    Ok(Value::fixnum(ns))
}

// ----- sleep -----

fn sleep_ms(args: &[Value]) -> Result<Value, FfiError> {
    let ms = expect_fixnum("sleep-ms", args, 0)?;
    if ms < 0 {
        return Err(FfiError::HostFailure("sleep-ms: negative duration".into()));
    }
    std::thread::sleep(Duration::from_millis(ms as u64));
    Ok(Value::Unspecified)
}

// ----- format / parse -----

fn format_time(args: &[Value]) -> Result<Value, FfiError> {
    let epoch_secs = expect_fixnum("format-time", args, 0)?;
    let fmt = expect_string("format-time", args, 1)?;
    let dt: DateTime<Utc> = match Utc.timestamp_opt(epoch_secs, 0).single() {
        Some(d) => d,
        None => {
            return Err(FfiError::HostFailure(format!(
                "format-time: epoch {} out of range",
                epoch_secs
            )));
        }
    };
    Ok(string_value(dt.format(&fmt).to_string()))
}

fn parse_time(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("parse-time", args, 0)?;
    let fmt = expect_string("parse-time", args, 1)?;
    match NaiveDateTime::parse_from_str(&s, &fmt) {
        Ok(naive) => Ok(Value::fixnum(naive.and_utc().timestamp())),
        Err(_) => Ok(Value::Boolean(false)),
    }
}
