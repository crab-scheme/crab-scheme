//! CrabScheme stdlib module: `(crab path)`.
//!
//! Pure path manipulation on top of `std::path` — no filesystem
//! access. Companion crate `cs-stdlib-fs` handles I/O.
//!
//! Iter 2 of the `stdlib-modules` spec. Each registered procedure
//! is the user-facing Scheme name (no `__crab-…` prefix indirection
//! yet — the wrapper-based design lands when the module loader
//! does).
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `path-join`            | string ... | string | Concatenates with the platform separator. |
//! | `path-basename`        | string     | string | Last component. |
//! | `path-dirname`         | string     | string | All but the last component, or "" if none. |
//! | `path-extension`       | string     | string | Without leading dot; "" if absent. |
//! | `path-stem`            | string     | string | Basename without extension. |
//! | `path-is-absolute?`    | string     | bool   |
//! | `path-with-extension`  | string string | string | Replace or add extension (pass "" to strip). |
//! | `path-components`      | string     | list of strings | Path split on the platform separator. |

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

/// Every `(crab path)` procedure as a Vec of `HostProcedure`
/// factories. cs-runtime iterates this and calls
/// `register_host_procedure` per entry — keeping the dep flow
/// one-way (cs-runtime → cs-stdlib-path, not back).
pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("path-join", path_join),
        UntypedProc::new("path-basename", path_basename),
        UntypedProc::new("path-dirname", path_dirname),
        UntypedProc::new("path-extension", path_extension),
        UntypedProc::new("path-stem", path_stem),
        UntypedProc::new("path-is-absolute?", path_is_absolute_p),
        UntypedProc::new("path-with-extension", path_with_extension),
        UntypedProc::new("path-components", path_components),
    ]
}

// ----- helpers -----

fn expect_string<'a>(name: &str, args: &'a [Value], idx: usize) -> Result<String, FfiError> {
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

fn string_value(s: impl Into<String>) -> Value {
    Value::string(s)
}

// ----- procedures -----

fn path_join(args: &[Value]) -> Result<Value, FfiError> {
    if args.is_empty() {
        return Err(FfiError::ArityError {
            name: "path-join".into(),
            expected: "at least 1".into(),
            got: 0,
        });
    }
    let mut buf = PathBuf::from(expect_string("path-join", args, 0)?);
    for (i, _) in args.iter().enumerate().skip(1) {
        buf.push(expect_string("path-join", args, i)?);
    }
    Ok(string_value(buf.to_string_lossy().into_owned()))
}

fn path_basename(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("path-basename", args, 0)?;
    let base = Path::new(&s)
        .file_name()
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(string_value(base))
}

fn path_dirname(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("path-dirname", args, 0)?;
    let dir = Path::new(&s)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(string_value(dir))
}

fn path_extension(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("path-extension", args, 0)?;
    let ext = Path::new(&s)
        .extension()
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(string_value(ext))
}

fn path_stem(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("path-stem", args, 0)?;
    let stem = Path::new(&s)
        .file_stem()
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(string_value(stem))
}

fn path_is_absolute_p(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("path-is-absolute?", args, 0)?;
    Ok(Value::Boolean(Path::new(&s).is_absolute()))
}

fn path_with_extension(args: &[Value]) -> Result<Value, FfiError> {
    let base = expect_string("path-with-extension", args, 0)?;
    let ext = expect_string("path-with-extension", args, 1)?;
    Ok(string_value(
        Path::new(&base)
            .with_extension(&ext)
            .to_string_lossy()
            .into_owned(),
    ))
}

fn path_components(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("path-components", args, 0)?;
    let parts: Vec<Value> = Path::new(&s)
        .components()
        .map(|c| string_value(c.as_os_str().to_string_lossy().into_owned()))
        .collect();
    Ok(Value::list(parts))
}
