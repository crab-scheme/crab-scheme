//! CrabScheme stdlib module: `(crab os)`.
//!
//! Environment variables, working directory, process identity,
//! platform info. Iter 3 of the `stdlib-modules` spec.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `get-env`          | string         | string or #f | `#f` when unset. |
//! | `set-env!`         | string string  | unspec       |
//! | `unset-env!`       | string         | unspec       |
//! | `env-vars`         | —              | list of (k . v) pairs | UTF-8 names + values only; non-UTF-8 entries are skipped. |
//! | `current-directory`| —              | string       |
//! | `change-directory` | string         | unspec       |
//! | `process-id`       | —              | fixnum       | Current process PID. |
//! | `parent-process-id`| —              | fixnum       | Parent PID. |
//! | `hostname`         | —              | string       |
//! | `username`         | —              | string or #f | Reads `USER` / `LOGNAME` / `USERNAME`. |
//! | `platform`         | —              | string       | E.g. `"linux"`, `"macos"`, `"windows"`. |
//! | `architecture`     | —              | string       | E.g. `"x86_64"`, `"aarch64"`. |
//! | `exit`             | fixnum (opt)   | never        | Default exit code 0. |

use std::env;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("get-env", get_env),
        UntypedProc::new("set-env!", set_env),
        UntypedProc::new("unset-env!", unset_env),
        UntypedProc::new("env-vars", env_vars),
        UntypedProc::new("current-directory", current_directory),
        UntypedProc::new("change-directory", change_directory),
        UntypedProc::new("process-id", process_id),
        UntypedProc::new("parent-process-id", parent_process_id),
        UntypedProc::new("hostname", hostname),
        UntypedProc::new("username", username),
        UntypedProc::new("platform", platform),
        UntypedProc::new("architecture", architecture),
        UntypedProc::new("exit", exit_proc),
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

fn expect_no_args(name: &str, args: &[Value]) -> Result<(), FfiError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(FfiError::ArityError {
            name: name.into(),
            expected: "0".into(),
            got: args.len(),
        })
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

// ----- environment -----

fn get_env(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("get-env", args, 0)?;
    Ok(env::var(&name).map_or(Value::Boolean(false), string_value))
}

fn set_env(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("set-env!", args, 0)?;
    let value = expect_string("set-env!", args, 1)?;
    // SAFETY: env::set_var is unsafe as of Rust 1.86 because it can
    // race with reads from other threads. CrabScheme's runtime is
    // single-threaded; the only place env can change concurrently is
    // outside the runtime, which the user accepts when calling this.
    unsafe { env::set_var(&name, &value) };
    Ok(Value::Unspecified)
}

fn unset_env(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("unset-env!", args, 0)?;
    // SAFETY: see `set_env` above.
    unsafe { env::remove_var(&name) };
    Ok(Value::Unspecified)
}

fn env_vars(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("env-vars", args)?;
    let pairs: Vec<Value> = env::vars()
        .map(|(k, v)| Value::Pair(cs_core::Pair::new(string_value(k), string_value(v))))
        .collect();
    Ok(Value::list(pairs))
}

// ----- working directory -----

fn current_directory(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("current-directory", args)?;
    env::current_dir()
        .map(|p| string_value(p.to_string_lossy().into_owned()))
        .map_err(|e| FfiError::HostFailure(format!("current-directory: {}", e)))
}

fn change_directory(args: &[Value]) -> Result<Value, FfiError> {
    let p = expect_string("change-directory", args, 0)?;
    env::set_current_dir(&p)
        .map(|_| Value::Unspecified)
        .map_err(|e| FfiError::HostFailure(format!("change-directory: {}: {}", p, e)))
}

// ----- process identity -----

fn process_id(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("process-id", args)?;
    Ok(Value::fixnum(std::process::id() as i64))
}

fn parent_process_id(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("parent-process-id", args)?;
    // std::os::unix::process::parent_id is stable on Unix; on Windows
    // there's no direct std API, so report 0 there.
    #[cfg(unix)]
    {
        Ok(Value::fixnum(std::os::unix::process::parent_id() as i64))
    }
    #[cfg(not(unix))]
    {
        Ok(Value::fixnum(0))
    }
}

// ----- host identity -----

#[cfg(not(target_family = "wasm"))]
fn hostname(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("hostname", args)?;
    let os = gethostname::gethostname();
    Ok(string_value(os.to_string_lossy().into_owned()))
}

// WASI preview 1 has no hostname syscall. Fall back to the
// HOSTNAME env var (which the runtime may have injected) and
// otherwise return the literal "wasi" so callers always get a
// non-empty answer.
#[cfg(target_family = "wasm")]
fn hostname(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("hostname", args)?;
    let name = std::env::var("HOSTNAME").unwrap_or_else(|_| "wasi".to_string());
    Ok(string_value(name))
}

fn username(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("username", args)?;
    // Cross-platform-ish fallback chain. Unix exports USER (and most
    // shells export LOGNAME too); Windows exports USERNAME. None
    // present → return #f, matching the get-env pattern.
    let candidate = ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|var| env::var(var).ok());
    Ok(candidate.map_or(Value::Boolean(false), string_value))
}

// ----- platform -----

fn platform(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("platform", args)?;
    // env::consts::OS is a `&'static str` like "linux", "macos",
    // "windows", "freebsd". Returned as a string until the FFI
    // layer gains a SymbolTable-aware symbol constructor.
    Ok(string_value(env::consts::OS))
}

fn architecture(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("architecture", args)?;
    Ok(string_value(env::consts::ARCH))
}

// ----- exit -----

fn exit_proc(args: &[Value]) -> Result<Value, FfiError> {
    let code = match args.first() {
        None => 0,
        Some(Value::Number(n)) => n.to_f64() as i32,
        Some(other) => {
            return Err(FfiError::TypeMismatch {
                expected: "fixnum or no args".into(),
                got: other.type_name().to_string(),
            });
        }
    };
    std::process::exit(code);
}
