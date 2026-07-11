//! CrabScheme stdlib module: `(crab toml)`.
//!
//! TOML read/write via the `toml` Rust crate. Iter 6 of the
//! `stdlib-modules` spec.
//!
//! Mapping mirrors `(crab json)`:
//!
//! | TOML                | Scheme                                  |
//! |---------------------|-----------------------------------------|
//! | table               | alist (list of (key . value) pairs)     |
//! | array               | Scheme list                             |
//! | string              | string                                  |
//! | integer             | fixnum                                  |
//! | float               | flonum                                  |
//! | boolean             | `#t` / `#f`                             |
//! | datetime            | string (RFC 3339 form)                  |
//!
//! Datetimes round-trip as strings — typed datetime values land
//! with `Value::Opaque`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `toml-parse`     | string | scheme value | Errors on malformed input. |
//! | `toml-stringify` | scheme value | string |

use std::sync::Arc;

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use toml::Value as T;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("toml-parse", toml_parse),
        UntypedProc::new("toml-stringify", toml_stringify),
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
    Value::string(s)
}

// ----- toml → scheme -----

fn toml_to_value(t: &T) -> Value {
    match t {
        T::String(s) => string_value(s.clone()),
        T::Integer(i) => Value::fixnum(*i),
        T::Float(f) => Value::flonum(*f),
        T::Boolean(b) => Value::Boolean(*b),
        T::Datetime(d) => string_value(d.to_string()),
        T::Array(items) => Value::list(items.iter().map(toml_to_value)),
        T::Table(map) => Value::list(
            map.iter()
                .map(|(k, v)| Value::Pair(Pair::new(string_value(k.clone()), toml_to_value(v)))),
        ),
    }
}

// ----- scheme → toml -----

fn value_to_toml(v: &Value) -> Result<T, FfiError> {
    match v {
        Value::Boolean(b) => Ok(T::Boolean(*b)),
        Value::Number(n) => {
            let f = n.to_f64();
            if f.fract() == 0.0 && f.is_finite() && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                Ok(T::Integer(f as i64))
            } else {
                Ok(T::Float(f))
            }
        }
        Value::String(s) => Ok(T::String(s.borrow().clone())),
        Value::Null => Ok(T::Array(Vec::new())),
        Value::Pair(_) => {
            let items = collect_list(v)?;
            if items
                .iter()
                .all(|item| matches!(item, Value::Pair(p) if matches!(p.car(), Value::String(_))))
                && !items.is_empty()
            {
                // Encode as table.
                let mut table = toml::map::Map::new();
                for item in &items {
                    if let Value::Pair(p) = item {
                        let key = match p.car() {
                            Value::String(s) => s.borrow().clone(),
                            _ => unreachable!("alist guard above"),
                        };
                        table.insert(key, value_to_toml(&p.cdr())?);
                    }
                }
                Ok(T::Table(table))
            } else {
                let mut out = Vec::with_capacity(items.len());
                for item in &items {
                    out.push(value_to_toml(item)?);
                }
                Ok(T::Array(out))
            }
        }
        other => Err(FfiError::TypeMismatch {
            expected: "TOML-encodable value".into(),
            got: other.type_name().to_string(),
        }),
    }
}

fn collect_list(v: &Value) -> Result<Vec<Value>, FfiError> {
    let mut cur = v.clone();
    let mut out = Vec::new();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(p.car());
                cur = p.cdr();
            }
            other => {
                return Err(FfiError::TypeMismatch {
                    expected: "proper list".into(),
                    got: other.type_name().to_string(),
                })
            }
        }
    }
}

// ----- procedures -----

fn toml_parse(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("toml-parse", args, 0)?;
    let parsed: T =
        toml::from_str(&s).map_err(|e| FfiError::HostFailure(format!("toml-parse: {}", e)))?;
    Ok(toml_to_value(&parsed))
}

fn toml_stringify(args: &[Value]) -> Result<Value, FfiError> {
    let v = args.first().ok_or(arity("toml-stringify", ">= 1", 0))?;
    let t = value_to_toml(v)?;
    // `toml::to_string` requires the top level to be a table. If a
    // caller hands us a scalar / array we wrap it in a synthetic
    // single-key table named "value" rather than erroring — keeps the
    // procedure total over any TOML-encodable input.
    let s = if matches!(t, T::Table(_)) {
        toml::to_string(&t).map_err(|e| FfiError::HostFailure(format!("toml-stringify: {}", e)))?
    } else {
        let mut wrap = toml::map::Map::new();
        wrap.insert("value".into(), t);
        toml::to_string(&T::Table(wrap))
            .map_err(|e| FfiError::HostFailure(format!("toml-stringify: {}", e)))?
    };
    Ok(string_value(s))
}
