//! CrabScheme stdlib module: `(crab log)`.
//!
//! Leveled stderr logging. Iter 8 of the `stdlib-modules` spec.
//!
//! Deliberately small — no global subscriber to install (which
//! would constrain WASM / embed builds), no per-call allocations
//! beyond what `format!` does. The output format is one line per
//! call: `<UNIX_EPOCH_MS> <LEVEL> <MSG>` on stderr.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `log-trace`        | message…        | unspec  | Threshold off by default — only emitted when level is `trace`. |
//! | `log-debug`        | message…        | unspec  | Emitted when level is `debug` or finer. |
//! | `log-info`         | message…        | unspec  | Emitted when level is `info` or finer. Default level. |
//! | `log-warn`         | message…        | unspec  | Emitted at `warn` or finer. |
//! | `log-error`        | message…        | unspec  | Always emitted (unless level is `off`). |
//! | `log-set-level!`   | symbol-or-string | unspec  | One of `'off`, `'error`, `'warn`, `'info`, `'debug`, `'trace`. |
//! | `log-current-level`| —               | string  | Current level as a string. |
//!
//! Variadic message args are rendered using each arg's display
//! form, joined with a single space.

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

// Level encoding: numeric severity, lower = more verbose. The
// threshold says "emit if call severity >= threshold". The
// `Off` value sits above everything.
const LVL_TRACE: u8 = 0;
const LVL_DEBUG: u8 = 1;
const LVL_INFO: u8 = 2;
const LVL_WARN: u8 = 3;
const LVL_ERROR: u8 = 4;
const LVL_OFF: u8 = 5;

static LEVEL: AtomicU8 = AtomicU8::new(LVL_INFO);

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("log-trace", |a| log_at(LVL_TRACE, "TRACE", a)),
        UntypedProc::new("log-debug", |a| log_at(LVL_DEBUG, "DEBUG", a)),
        UntypedProc::new("log-info", |a| log_at(LVL_INFO, "INFO", a)),
        UntypedProc::new("log-warn", |a| log_at(LVL_WARN, "WARN", a)),
        UntypedProc::new("log-error", |a| log_at(LVL_ERROR, "ERROR", a)),
        UntypedProc::new("log-set-level!", log_set_level),
        UntypedProc::new("log-current-level", log_current_level),
    ]
}

// ----- helpers -----

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn render(args: &[Value]) -> String {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match a {
            Value::String(s) => out.push_str(&s.borrow()),
            Value::Number(n) => out.push_str(&format!("{}", n.to_f64())),
            Value::Boolean(true) => out.push_str("#t"),
            Value::Boolean(false) => out.push_str("#f"),
            Value::Null => out.push_str("()"),
            Value::Character(c) => out.push(*c),
            other => out.push_str(&format!("<{}>", other.type_name())),
        }
    }
    out
}

fn log_at(call_level: u8, label: &'static str, args: &[Value]) -> Result<Value, FfiError> {
    let threshold = LEVEL.load(Ordering::Relaxed);
    if call_level < threshold {
        return Ok(Value::Unspecified);
    }
    let line = format!("{} {} {}", epoch_ms(), label, render(args));
    let stderr = std::io::stderr();
    let mut h = stderr.lock();
    let _ = writeln!(h, "{}", line);
    let _ = h.flush();
    Ok(Value::Unspecified)
}

fn parse_level(s: &str) -> Option<u8> {
    match s {
        "off" => Some(LVL_OFF),
        "error" => Some(LVL_ERROR),
        "warn" => Some(LVL_WARN),
        "info" => Some(LVL_INFO),
        "debug" => Some(LVL_DEBUG),
        "trace" => Some(LVL_TRACE),
        _ => None,
    }
}

fn level_name(v: u8) -> &'static str {
    match v {
        LVL_OFF => "off",
        LVL_ERROR => "error",
        LVL_WARN => "warn",
        LVL_INFO => "info",
        LVL_DEBUG => "debug",
        _ => "trace",
    }
}

fn log_set_level(args: &[Value]) -> Result<Value, FfiError> {
    let name: String = match args.first() {
        Some(Value::String(s)) => s.borrow().clone(),
        // Symbols come through as fixnum-ish; we don't have access to
        // SymbolTable, so accept strings only for now. Future iter
        // can widen to symbols when an opaque-payload-or-symbol
        // helper exists.
        Some(other) => {
            return Err(FfiError::TypeMismatch {
                expected: "string",
                got: other.type_name().to_string(),
            })
        }
        None => {
            return Err(FfiError::ArityError {
                name: "log-set-level!".into(),
                expected: "1".into(),
                got: 0,
            })
        }
    };
    let lvl = parse_level(&name).ok_or_else(|| {
        FfiError::HostFailure(format!(
            "log-set-level!: unknown level {:?} (want off/error/warn/info/debug/trace)",
            name
        ))
    })?;
    LEVEL.store(lvl, Ordering::Relaxed);
    Ok(Value::Unspecified)
}

fn log_current_level(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(FfiError::ArityError {
            name: "log-current-level".into(),
            expected: "0".into(),
            got: args.len(),
        });
    }
    Ok(Value::string(
        level_name(LEVEL.load(Ordering::Relaxed)).to_string(),
    ))
}
