//! CrabScheme stdlib module: `(crab process)`.
//!
//! Synchronous subprocess execution. Iter 3 of the
//! `stdlib-modules` spec.
//!
//! Async / spawn-and-wait-later patterns (returning a process
//! handle to be passed to a separate `process-wait` call) need an
//! opaque-payload Scheme value the FFI layer doesn't yet support;
//! they're tracked for a follow-up iter once `Value::Opaque`
//! lands. This iter ships the synchronous convenience:
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `run`   | string (list-of-strings) [string (stdin)] | list `(exit-code stdout stderr)` | Runs to completion; stdout/stderr captured as strings. |
//! | `run/status` | string (list-of-strings) | fixnum  | Like `run`, but inherits stdio; returns only the exit code. |
//! | `which` | string | string or #f | First PATH match for `cmd`; `#f` if not found. |

use std::process::{Command, Stdio};
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("run", run_proc),
        UntypedProc::new("run/status", run_status_proc),
        UntypedProc::new("which", which_proc),
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

fn expect_string_list(name: &str, args: &[Value], idx: usize) -> Result<Vec<String>, FfiError> {
    let mut cur = match args.get(idx) {
        Some(v) => v.clone(),
        None => {
            return Err(FfiError::ArityError {
                name: name.into(),
                expected: format!("at least {} args", idx + 1),
                got: args.len(),
            })
        }
    };
    let mut out = Vec::new();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(pair) => {
                let car = pair.car();
                match car {
                    Value::String(s) => out.push(s.borrow().clone()),
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
    Ok(out)
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

// ----- run -----

fn run_proc(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(FfiError::ArityError {
            name: "run".into(),
            expected: "2 or 3".into(),
            got: args.len(),
        });
    }
    let cmd = expect_string("run", args, 0)?;
    let argv = expect_string_list("run", args, 1)?;
    let stdin_payload = if args.len() == 3 {
        Some(expect_string("run", args, 2)?)
    } else {
        None
    };

    let mut command = Command::new(&cmd);
    command.args(&argv);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if stdin_payload.is_some() {
        command.stdin(Stdio::piped());
    }

    let mut child = command
        .spawn()
        .map_err(|e| FfiError::HostFailure(format!("run: spawn {}: {}", cmd, e)))?;

    if let Some(payload) = stdin_payload {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(payload.as_bytes())
                .map_err(|e| FfiError::HostFailure(format!("run: write stdin {}: {}", cmd, e)))?;
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| FfiError::HostFailure(format!("run: wait {}: {}", cmd, e)))?;

    let exit_code = output.status.code().unwrap_or(-1) as i64;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Ok(Value::list([
        Value::fixnum(exit_code),
        string_value(stdout),
        string_value(stderr),
    ]))
}

fn run_status_proc(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(FfiError::ArityError {
            name: "run/status".into(),
            expected: "2".into(),
            got: args.len(),
        });
    }
    let cmd = expect_string("run/status", args, 0)?;
    let argv = expect_string_list("run/status", args, 1)?;
    let status = Command::new(&cmd)
        .args(&argv)
        .status()
        .map_err(|e| FfiError::HostFailure(format!("run/status: {}: {}", cmd, e)))?;
    Ok(Value::fixnum(status.code().unwrap_or(-1) as i64))
}

// ----- which -----

fn which_proc(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(FfiError::ArityError {
            name: "which".into(),
            expected: "1".into(),
            got: args.len(),
        });
    }
    let cmd = expect_string("which", args, 0)?;
    match which::which(&cmd) {
        Ok(path) => Ok(string_value(path.to_string_lossy().into_owned())),
        Err(_) => Ok(Value::Boolean(false)),
    }
}
