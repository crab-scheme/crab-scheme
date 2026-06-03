//! CrabScheme stdlib module: `(crab ini)`.
//!
//! INI configuration parsing and serialization — the `(crab …)` answer
//! to Python's `configparser`. Pure Rust, no dependencies, wasm-portable.
//!
//! ## Representation
//!
//! An INI document is a section alist: `((section . ((key . value) …)) …)`
//! where sections, keys, and values are all strings. Keys appearing
//! before any `[section]` header live under the empty-string section
//! `""`. Comment lines start with `;` or `#`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `ini-parse`     | string                  | section alist | |
//! | `ini-stringify` | section-alist           | string        | |
//! | `ini-ref`       | data section key [default] | string or default | Lookup helper. |
//!
//! ```scheme
//! (import (crab ini))
//! (define cfg (ini-parse "[server]\nhost = localhost\nport = 8080\n"))
//! (ini-ref cfg "server" "port")          ; => "8080"
//! (ini-ref cfg "server" "missing" "x")   ; => "x"
//! ```

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("ini-parse", ini_parse),
        UntypedProc::new("ini-stringify", ini_stringify),
        UntypedProc::new("ini-ref", ini_ref),
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
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn sval(s: impl Into<String>) -> Value {
    Value::string(s)
}

fn read_list(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    while let Value::Pair(p) = cur {
        out.push(p.car());
        cur = p.cdr();
    }
    out
}

fn as_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.borrow().clone()),
        _ => None,
    }
}

// ----- parse -----

fn ini_parse(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("ini-parse", "1", args.len()));
    }
    let src = expect_string("ini-parse", args, 0)?;
    // Sections in first-seen order; each holds (key . value) pairs.
    let mut sections: Vec<(String, Vec<(String, String)>)> = vec![(String::new(), Vec::new())];
    let mut cur = 0usize;
    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_string();
            match sections.iter().position(|(n, _)| *n == name) {
                Some(i) => cur = i,
                None => {
                    sections.push((name, Vec::new()));
                    cur = sections.len() - 1;
                }
            }
        } else if let Some(eq) = line.find('=') {
            let key = line[..eq].trim().to_string();
            let val = line[eq + 1..].trim().to_string();
            sections[cur].1.push((key, val));
        }
        // lines without '=' and not a section header are ignored
    }
    // Drop the empty default section if it has no keys.
    let entries: Vec<Value> = sections
        .into_iter()
        .filter(|(n, kv)| !(n.is_empty() && kv.is_empty()))
        .map(|(name, kv)| {
            let pairs: Vec<Value> = kv
                .into_iter()
                .map(|(k, v)| Value::Pair(cs_core::Pair::new(sval(k), sval(v))))
                .collect();
            Value::Pair(cs_core::Pair::new(sval(name), Value::list(pairs)))
        })
        .collect();
    Ok(Value::list(entries))
}

// ----- stringify -----

fn ini_stringify(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("ini-stringify", "1", args.len()));
    }
    let mut out = String::new();
    for section in read_list(&args[0]) {
        let (name, pairs) = match section {
            Value::Pair(p) => (p.car(), p.cdr()),
            other => {
                return Err(FfiError::HostFailure(format!(
                    "ini-stringify: each section must be a (name . pairs) pair, got {}",
                    other.type_name()
                )))
            }
        };
        let name = as_str(&name).ok_or_else(|| {
            FfiError::HostFailure("ini-stringify: section name must be a string".into())
        })?;
        if !name.is_empty() {
            out.push('[');
            out.push_str(&name);
            out.push_str("]\n");
        }
        for kv in read_list(&pairs) {
            if let Value::Pair(p) = kv {
                let k = as_str(&p.car()).ok_or_else(|| {
                    FfiError::HostFailure("ini-stringify: key must be a string".into())
                })?;
                let v = as_str(&p.cdr()).ok_or_else(|| {
                    FfiError::HostFailure("ini-stringify: value must be a string".into())
                })?;
                out.push_str(&k);
                out.push_str(" = ");
                out.push_str(&v);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    Ok(sval(out))
}

// ----- lookup -----

fn ini_ref(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(arity("ini-ref", "3 or 4", args.len()));
    }
    let section = expect_string("ini-ref", args, 1)?;
    let key = expect_string("ini-ref", args, 2)?;
    let default = args.get(3).cloned().unwrap_or(Value::Boolean(false));
    for sec in read_list(&args[0]) {
        if let Value::Pair(p) = sec {
            if as_str(&p.car()).as_deref() == Some(section.as_str()) {
                for kv in read_list(&p.cdr()) {
                    if let Value::Pair(e) = kv {
                        if as_str(&e.car()).as_deref() == Some(key.as_str()) {
                            return Ok(e.cdr());
                        }
                    }
                }
            }
        }
    }
    Ok(default)
}
