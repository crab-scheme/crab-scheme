//! CrabScheme stdlib module: `(crab string)`.
//!
//! String operations beyond the R6RS basics (`string-length`,
//! `substring`, `string-append`, …). Iter 4 of the
//! `stdlib-modules` spec.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `string-split`        | str sep            | list of strings | Empty `sep` splits on every char. |
//! | `string-join`         | list-of-strings sep| string          |
//! | `string-trim`         | str                | string          | Whitespace both sides. |
//! | `string-trim-left`    | str                | string          |
//! | `string-trim-right`   | str                | string          |
//! | `string-replace`      | str old new        | string          | All occurrences. |
//! | `string-contains?`    | str needle         | boolean         |
//! | `string-starts-with?` | str prefix         | boolean         |
//! | `string-ends-with?`   | str suffix         | boolean         |
//! | `string-pad-left`     | str width [ch]     | string          | Default fill: space. |
//! | `string-pad-right`    | str width [ch]     | string          |
//! | `string-repeat`       | str n              | string          |
//!
//! Indices and lengths are in chars/bytes per the underlying `str`
//! methods — these mostly delegate to `str::split`, `trim`,
//! `replace`, `contains`, `starts_with`, `ends_with`. Surrogate-pair
//! / grapheme-cluster-aware variants (`string-grapheme-count`, etc.)
//! are deferred to a follow-up iter that pulls in
//! `unicode-segmentation`.

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("string-split", string_split),
        UntypedProc::new("string-join", string_join),
        UntypedProc::new("string-trim", string_trim),
        UntypedProc::new("string-trim-left", string_trim_left),
        UntypedProc::new("string-trim-right", string_trim_right),
        UntypedProc::new("string-replace", string_replace),
        UntypedProc::new("string-contains?", string_contains_p),
        UntypedProc::new("string-starts-with?", string_starts_with_p),
        UntypedProc::new("string-ends-with?", string_ends_with_p),
        UntypedProc::new("string-pad-left", string_pad_left),
        UntypedProc::new("string-pad-right", string_pad_right),
        UntypedProc::new("string-repeat", string_repeat),
    ]
}

// ----- helpers -----

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(FfiError::ArityError {
            name: name.into(),
            expected: format!("at least {} args", idx + 1),
            got: args.len(),
        }),
    }
}

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Number(cs_core::Number::Fixnum(v))) => Ok(*v),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(FfiError::ArityError {
            name: name.into(),
            expected: format!("at least {} args", idx + 1),
            got: args.len(),
        }),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

// ----- procedures -----

fn string_split(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-split", args, 0)?;
    let sep = expect_string("string-split", args, 1)?;
    let parts: Vec<Value> = if sep.is_empty() {
        s.chars().map(|c| string_value(c.to_string())).collect()
    } else {
        s.split(&sep).map(string_value).collect()
    };
    Ok(Value::list(parts))
}

fn string_join(args: &[Value]) -> Result<Value, FfiError> {
    // First arg: a Scheme list of strings.
    let mut cur = args.first().cloned().ok_or(FfiError::ArityError {
        name: "string-join".into(),
        expected: "at least 1".into(),
        got: 0,
    })?;
    let sep = expect_string("string-join", args, 1)?;

    let mut parts: Vec<String> = Vec::new();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(pair) => {
                match pair.car() {
                    Value::String(s) => parts.push(s.borrow().clone()),
                    other => {
                        return Err(FfiError::TypeMismatch {
                            expected: "list of strings".into(),
                            got: format!("list containing {}", other.type_name()),
                        })
                    }
                }
                cur = pair.cdr();
            }
            other => {
                return Err(FfiError::TypeMismatch {
                    expected: "proper list of strings".into(),
                    got: other.type_name().to_string(),
                })
            }
        }
    }
    Ok(string_value(parts.join(&sep)))
}

fn string_trim(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-trim", args, 0)?;
    Ok(string_value(s.trim().to_string()))
}

fn string_trim_left(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-trim-left", args, 0)?;
    Ok(string_value(s.trim_start().to_string()))
}

fn string_trim_right(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-trim-right", args, 0)?;
    Ok(string_value(s.trim_end().to_string()))
}

fn string_replace(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-replace", args, 0)?;
    let old = expect_string("string-replace", args, 1)?;
    let new = expect_string("string-replace", args, 2)?;
    Ok(string_value(s.replace(&old, &new)))
}

fn string_contains_p(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-contains?", args, 0)?;
    let needle = expect_string("string-contains?", args, 1)?;
    Ok(Value::Boolean(s.contains(&needle)))
}

fn string_starts_with_p(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-starts-with?", args, 0)?;
    let prefix = expect_string("string-starts-with?", args, 1)?;
    Ok(Value::Boolean(s.starts_with(&prefix)))
}

fn string_ends_with_p(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-ends-with?", args, 0)?;
    let suffix = expect_string("string-ends-with?", args, 1)?;
    Ok(Value::Boolean(s.ends_with(&suffix)))
}

fn pad_fill_char(name: &str, args: &[Value], idx: usize) -> Result<char, FfiError> {
    match args.get(idx) {
        None => Ok(' '),
        Some(Value::Character(c)) => Ok(*c),
        Some(Value::String(s)) => s
            .borrow()
            .chars()
            .next()
            .ok_or_else(|| FfiError::HostFailure(format!("{}: empty fill string", name))),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "char or single-char string".into(),
            got: other.type_name().to_string(),
        }),
    }
}

fn string_pad_left(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-pad-left", args, 0)?;
    let width = expect_fixnum("string-pad-left", args, 1)? as usize;
    let fill = pad_fill_char("string-pad-left", args, 2)?;
    let cur = s.chars().count();
    if cur >= width {
        return Ok(string_value(s));
    }
    let pad = fill.to_string().repeat(width - cur);
    Ok(string_value(format!("{}{}", pad, s)))
}

fn string_pad_right(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-pad-right", args, 0)?;
    let width = expect_fixnum("string-pad-right", args, 1)? as usize;
    let fill = pad_fill_char("string-pad-right", args, 2)?;
    let cur = s.chars().count();
    if cur >= width {
        return Ok(string_value(s));
    }
    let pad = fill.to_string().repeat(width - cur);
    Ok(string_value(format!("{}{}", s, pad)))
}

fn string_repeat(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("string-repeat", args, 0)?;
    let n = expect_fixnum("string-repeat", args, 1)?;
    if n < 0 {
        return Err(FfiError::HostFailure(
            "string-repeat: negative count".into(),
        ));
    }
    Ok(string_value(s.repeat(n as usize)))
}
