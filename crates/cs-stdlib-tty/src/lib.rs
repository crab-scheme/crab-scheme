//! CrabScheme stdlib module: `(crab tty)`.
//!
//! Terminal detection + size query. Iter 13 of the
//! `stdlib-modules` spec.
//!
//! Cursor movement, color control, raw-mode toggling, and
//! interactive line editing all need a richer crate (`crossterm` /
//! `console`) and design discussion; deferred. This iter ships
//! the small set of "is my output going to a terminal" /
//! "how wide is it" queries that every CLI tool reaches for.
//!
//! ## Registered procedures
//!
//! | Name | Args | Returns |
//! |---|---|---|
//! | `tty-stdin?`     | — | boolean |
//! | `tty-stdout?`    | — | boolean |
//! | `tty-stderr?`    | — | boolean |
//! | `terminal-size`  | — | `(cols rows)` or `#f` if no tty / unknown |

use std::io::IsTerminal;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use terminal_size::{terminal_size, Height, Width};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("tty-stdin?", tty_stdin_p),
        UntypedProc::new("tty-stdout?", tty_stdout_p),
        UntypedProc::new("tty-stderr?", tty_stderr_p),
        UntypedProc::new("terminal-size", terminal_size_proc),
    ]
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

fn tty_stdin_p(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("tty-stdin?", args)?;
    Ok(Value::Boolean(std::io::stdin().is_terminal()))
}

fn tty_stdout_p(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("tty-stdout?", args)?;
    Ok(Value::Boolean(std::io::stdout().is_terminal()))
}

fn tty_stderr_p(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("tty-stderr?", args)?;
    Ok(Value::Boolean(std::io::stderr().is_terminal()))
}

fn terminal_size_proc(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("terminal-size", args)?;
    Ok(match terminal_size() {
        Some((Width(w), Height(h))) => {
            Value::list(vec![Value::fixnum(w as i64), Value::fixnum(h as i64)])
        }
        None => Value::Boolean(false),
    })
}
