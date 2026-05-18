//! CrabScheme stdlib module: `(crab json)`.
//!
//! JSON encode/decode via `serde_json`. Iter 6 of the
//! `stdlib-modules` spec.
//!
//! ## Scheme ↔ JSON mapping
//!
//! | JSON                | Scheme                                  |
//! |---------------------|-----------------------------------------|
//! | object              | alist (list of (key . value) pairs)     |
//! | array               | Scheme list                             |
//! | string              | string                                  |
//! | number (integer)    | fixnum (if in i64 range; else flonum)   |
//! | number (fractional) | flonum                                  |
//! | true / false        | `#t` / `#f`                             |
//! | null                | `'()`                                   |
//!
//! `null` collides with the empty list/array on decode — both
//! become `'()`. Round-tripping `null` through Scheme yields `[]`
//! on re-encode. A typed `json-null?` sentinel lands when
//! `Value::Opaque` does.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `json-parse`     | string | scheme value | Errors on malformed input. |
//! | `json-stringify` | scheme value | string | Compact (no whitespace). |
//! | `json-pretty`    | scheme value | string | Two-space indent. |

use std::sync::Arc;

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use serde_json::Value as J;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("json-parse", json_parse),
        UntypedProc::new("json-stringify", json_stringify),
        UntypedProc::new("json-pretty", json_pretty),
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

// ----- json → scheme -----

fn json_to_value(j: &J) -> Value {
    match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::fixnum(i)
            } else if let Some(f) = n.as_f64() {
                Value::flonum(f)
            } else {
                // u64 outside i64 range — fall back to flonum.
                Value::flonum(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => string_value(s.clone()),
        J::Array(items) => Value::list(items.iter().map(json_to_value)),
        J::Object(map) => Value::list(
            map.iter()
                .map(|(k, v)| Value::Pair(Pair::new(string_value(k.clone()), json_to_value(v)))),
        ),
    }
}

// ----- scheme → json -----

fn value_to_json(v: &Value) -> Result<J, FfiError> {
    match v {
        Value::Null => Ok(J::Array(Vec::new())),
        Value::Boolean(b) => Ok(J::Bool(*b)),
        Value::Number(n) => {
            let f = n.to_f64();
            if f.fract() == 0.0 && f.is_finite() && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                Ok(J::Number((f as i64).into()))
            } else {
                serde_json::Number::from_f64(f)
                    .map(J::Number)
                    .ok_or_else(|| {
                        FfiError::HostFailure(format!(
                            "json-stringify: non-finite flonum {} not representable",
                            f
                        ))
                    })
            }
        }
        Value::String(s) => Ok(J::String(s.borrow().clone())),
        Value::Pair(_) => {
            // Distinguish alist (list of pairs whose cars are strings)
            // from list. Walk the list; if every element is a Pair
            // whose car is a String, encode as object; otherwise as
            // array.
            let items = collect_list(v)?;
            if items
                .iter()
                .all(|item| matches!(item, Value::Pair(p) if matches!(p.car(), Value::String(_))))
                && !items.is_empty()
            {
                let mut obj = serde_json::Map::with_capacity(items.len());
                for item in &items {
                    if let Value::Pair(p) = item {
                        let key = match p.car() {
                            Value::String(s) => s.borrow().clone(),
                            _ => unreachable!("alist guard above"),
                        };
                        let val = value_to_json(&p.cdr())?;
                        obj.insert(key, val);
                    }
                }
                Ok(J::Object(obj))
            } else {
                let mut out = Vec::with_capacity(items.len());
                for item in &items {
                    out.push(value_to_json(item)?);
                }
                Ok(J::Array(out))
            }
        }
        other => Err(FfiError::TypeMismatch {
            expected: "JSON-encodable value (null, bool, number, string, list, alist)".into(),
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

fn json_parse(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("json-parse", args, 0)?;
    let parsed: J = serde_json::from_str(&s)
        .map_err(|e| FfiError::HostFailure(format!("json-parse: {}", e)))?;
    Ok(json_to_value(&parsed))
}

fn json_stringify(args: &[Value]) -> Result<Value, FfiError> {
    let v = args.first().ok_or(arity("json-stringify", ">= 1", 0))?;
    let j = value_to_json(v)?;
    Ok(string_value(j.to_string()))
}

fn json_pretty(args: &[Value]) -> Result<Value, FfiError> {
    let v = args.first().ok_or(arity("json-pretty", ">= 1", 0))?;
    let j = value_to_json(v)?;
    Ok(string_value(serde_json::to_string_pretty(&j).map_err(
        |e| FfiError::HostFailure(format!("json-pretty: {}", e)),
    )?))
}
