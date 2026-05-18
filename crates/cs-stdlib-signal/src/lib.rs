//! CrabScheme stdlib module: `(crab signal)`.
//!
//! Unix signal polling via `signal-hook`. Iter 13 of the
//! `stdlib-modules` spec.
//!
//! Signal handlers can't invoke Scheme thunks directly — signal
//! contexts forbid most allocation, and CrabScheme's runtime is
//! single-threaded. Instead this module installs a flag-setting
//! handler at first `signal-watch!` call; subsequent
//! `(signal-poll)` returns the next pending signal name as a
//! string or `#f` if none arrived since the last poll.
//!
//! Standard pattern: arm the signals you care about once at
//! startup, then poll in your event loop.
//!
//! Windows builds (where `signal-hook` isn't available) register
//! stub procedures that always return `#f` from `signal-poll`
//! and accept any `signal-watch!` call as a no-op, so portable
//! programs don't need to `cond-expand`.
//!
//! ## Supported signal names (as strings)
//!
//! `"SIGINT"` / `"SIGTERM"` / `"SIGHUP"` / `"SIGQUIT"` /
//! `"SIGUSR1"` / `"SIGUSR2"`. Other signals raise.
//!
//! ## Registered procedures
//!
//! | Name | Args | Returns |
//! |---|---|---|
//! | `signal-watch!` | string | unspec |
//! | `signal-poll`   | —      | string or #f |

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("signal-watch!", signal_watch),
        UntypedProc::new("signal-poll", signal_poll),
    ]
}

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

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

#[cfg(unix)]
mod imp {
    use super::*;
    use signal_hook::consts as sig;
    use signal_hook::iterator::Signals;
    use std::collections::VecDeque;
    use std::sync::{Mutex, OnceLock};
    use std::thread;

    fn name_to_signum(name: &str) -> Option<i32> {
        match name {
            "SIGINT" => Some(sig::SIGINT),
            "SIGTERM" => Some(sig::SIGTERM),
            "SIGHUP" => Some(sig::SIGHUP),
            "SIGQUIT" => Some(sig::SIGQUIT),
            "SIGUSR1" => Some(sig::SIGUSR1),
            "SIGUSR2" => Some(sig::SIGUSR2),
            _ => None,
        }
    }

    fn signum_to_name(s: i32) -> Option<&'static str> {
        match s {
            x if x == sig::SIGINT => Some("SIGINT"),
            x if x == sig::SIGTERM => Some("SIGTERM"),
            x if x == sig::SIGHUP => Some("SIGHUP"),
            x if x == sig::SIGQUIT => Some("SIGQUIT"),
            x if x == sig::SIGUSR1 => Some("SIGUSR1"),
            x if x == sig::SIGUSR2 => Some("SIGUSR2"),
            _ => None,
        }
    }

    // Pending signals delivered into this queue by the
    // background reader thread; drained by signal-poll.
    fn queue() -> &'static Mutex<VecDeque<i32>> {
        static Q: OnceLock<Mutex<VecDeque<i32>>> = OnceLock::new();
        Q.get_or_init(|| Mutex::new(VecDeque::new()))
    }

    // Signals currently armed in the OS-level handler. Used to
    // avoid re-installing if the user calls signal-watch! twice.
    fn armed() -> &'static Mutex<Vec<i32>> {
        static A: OnceLock<Mutex<Vec<i32>>> = OnceLock::new();
        A.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// On first call to `signal-watch!` we install a background
    /// thread that reads from a `Signals` iterator and pushes
    /// into the shared queue. Subsequent calls add to the
    /// `Signals` set in place — but signal-hook doesn't expose
    /// add-after-construct cleanly, so we spawn one reader
    /// thread per watched signum. Threads are tiny (just a
    /// blocking `signals.forever()` loop).
    pub fn watch(name: &str) -> Result<(), String> {
        let signum = name_to_signum(name)
            .ok_or_else(|| format!("signal-watch!: unknown signal {}", name))?;
        let mut a = armed()
            .lock()
            .map_err(|e| format!("signal: armed poisoned: {}", e))?;
        if a.contains(&signum) {
            return Ok(());
        }
        let signals =
            Signals::new([signum]).map_err(|e| format!("signal-watch! {}: {}", name, e))?;
        thread::Builder::new()
            .name(format!("crab-signal-{}", name))
            .spawn(move || {
                let mut signals = signals;
                for s in signals.forever() {
                    if let Ok(mut q) = queue().lock() {
                        q.push_back(s);
                    }
                }
            })
            .map_err(|e| format!("signal-watch! spawn: {}", e))?;
        a.push(signum);
        Ok(())
    }

    pub fn poll() -> Option<&'static str> {
        let mut q = queue().lock().ok()?;
        let s = q.pop_front()?;
        signum_to_name(s)
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn watch(_name: &str) -> Result<(), String> {
        Ok(())
    }
    pub fn poll() -> Option<&'static str> {
        None
    }
}

fn signal_watch(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("signal-watch!", args, 0)?;
    imp::watch(&name).map_err(FfiError::HostFailure)?;
    Ok(Value::Unspecified)
}

fn signal_poll(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("signal-poll", "0", args.len()));
    }
    Ok(match imp::poll() {
        Some(name) => string_value(name.to_string()),
        None => Value::Boolean(false),
    })
}
