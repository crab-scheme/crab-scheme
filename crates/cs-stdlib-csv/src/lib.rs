//! CrabScheme stdlib module: `(crab csv)`.
//!
//! CSV read/write via the `csv` Rust crate. Iter 6 of the
//! `stdlib-modules` spec.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `csv-parse` | string         | list of (list of strings) | No header row treated specially. |
//! | `csv-write` | list-of-rows   | string                    | Each row is a list of strings. |

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("csv-parse", csv_parse),
        UntypedProc::new("csv-write", csv_write),
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

fn collect_string_list(v: &Value, ctx: &str) -> Result<Vec<String>, FfiError> {
    let mut cur = v.clone();
    let mut out = Vec::new();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                match p.car() {
                    Value::String(s) => out.push(s.borrow().clone()),
                    other => {
                        return Err(FfiError::HostFailure(format!(
                            "{}: expected list of strings; got element of type {}",
                            ctx,
                            other.type_name()
                        )));
                    }
                }
                cur = p.cdr();
            }
            other => {
                return Err(FfiError::HostFailure(format!(
                    "{}: expected proper list of strings; got {}",
                    ctx,
                    other.type_name()
                )));
            }
        }
    }
}

// ----- procedures -----

fn csv_parse(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("csv-parse", args, 0)?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(s.as_bytes());
    let mut rows: Vec<Value> = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| FfiError::HostFailure(format!("csv-parse: {}", e)))?;
        let cells: Vec<Value> = rec.iter().map(|c| string_value(c.to_string())).collect();
        rows.push(Value::list(cells));
    }
    Ok(Value::list(rows))
}

fn csv_write(args: &[Value]) -> Result<Value, FfiError> {
    let rows_val = args.first().cloned().ok_or(arity("csv-write", ">= 1", 0))?;

    // Walk outer list of rows.
    let mut wtr = csv::Writer::from_writer(Vec::new());
    let mut cur = rows_val;
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                let row = collect_string_list(&p.car(), "csv-write row")?;
                wtr.write_record(&row)
                    .map_err(|e| FfiError::HostFailure(format!("csv-write: {}", e)))?;
                cur = p.cdr();
            }
            other => {
                return Err(FfiError::TypeMismatch {
                    expected: "proper list of rows".into(),
                    got: other.type_name().to_string(),
                });
            }
        }
    }
    let buf = wtr
        .into_inner()
        .map_err(|e| FfiError::HostFailure(format!("csv-write: {}", e)))?;
    Ok(string_value(String::from_utf8(buf).map_err(|e| {
        FfiError::HostFailure(format!("csv-write: {}", e))
    })?))
}
