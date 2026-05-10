//! `HostProcedure` — the registration target.
//!
//! User Rust code provides values implementing this trait; the
//! runtime calls `register_host_procedure(arc)` to expose them
//! under their declared Scheme name.
//!
//! Iter 2 (this iter) ships only the trait + a couple of helper
//! adapters that wrap a closure or a typed function. The proc-macro
//! `#[host_proc("name")]` lands in iter 3 and emits one of the
//! adapters automatically.

use crate::error::FfiError;
use cs_core::Value;
use std::sync::Arc;

/// A Rust procedure callable from Scheme. The runtime stores
/// `Arc<dyn HostProcedure>` and dispatches via `call`.
///
/// `Send + Sync` because the runtime may, in a future iter, share
/// the procedure across threads (the immediate single-threaded use
/// case still requires Send for the registry vector).
pub trait HostProcedure: Send + Sync {
    /// Name the procedure is bound to in the Scheme top-level.
    fn name(&self) -> &str;

    /// Apply the procedure to its arguments. Returns either a Scheme
    /// `Value` or an `FfiError` that the runtime translates into a
    /// catchable condition.
    fn call(&self, args: &[Value]) -> Result<Value, FfiError>;
}

/// Untyped host procedure — takes raw `&[Value]`, returns a
/// `Result<Value, FfiError>`. Useful when the user wants full
/// control over argument shape (variadic procedures, etc.).
pub struct UntypedProc {
    name: String,
    f: Box<dyn Fn(&[Value]) -> Result<Value, FfiError> + Send + Sync>,
}

impl UntypedProc {
    pub fn new<F>(name: impl Into<String>, f: F) -> Arc<Self>
    where
        F: Fn(&[Value]) -> Result<Value, FfiError> + Send + Sync + 'static,
    {
        Arc::new(Self {
            name: name.into(),
            f: Box::new(f),
        })
    }
}

impl HostProcedure for UntypedProc {
    fn name(&self) -> &str {
        &self.name
    }

    fn call(&self, args: &[Value]) -> Result<Value, FfiError> {
        // catch_unwind around the user closure so a panic doesn't
        // abort the runtime. Translates the panic payload into
        // FfiError::Panic per ADR 0008 D-5.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.f)(args))) {
            Ok(r) => r,
            Err(payload) => Err(FfiError::Panic(panic_message(payload))),
        }
    }
}

/// Extract a printable message from a panic payload. `panic!("foo")`
/// stores its arg as a `&'static str` or `String`; anything else
/// prints generically.
fn panic_message(payload: Box<dyn std::any::Any + Send + 'static>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "panic with non-string payload".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marshal::IntoValue;
    use cs_core::Number;

    #[test]
    fn untyped_proc_dispatches() {
        let p = UntypedProc::new("inc", |args| {
            if args.len() != 1 {
                return Err(FfiError::ArityError {
                    name: "inc".into(),
                    expected: "1".into(),
                    got: args.len(),
                });
            }
            match &args[0] {
                Value::Number(Number::Fixnum(n)) => Ok((n + 1).into_value()),
                _ => Err(FfiError::TypeMismatch {
                    expected: "i64",
                    got: args[0].type_name().to_string(),
                }),
            }
        });
        assert_eq!(p.name(), "inc");
        let r = p.call(&[Value::fixnum(41)]).unwrap();
        match r {
            Value::Number(Number::Fixnum(42)) => {}
            other => panic!("expected 42, got {:?}", other),
        }
    }

    #[test]
    fn untyped_proc_arity_error() {
        let p = UntypedProc::new("must-have-one", |args| {
            if args.len() != 1 {
                return Err(FfiError::ArityError {
                    name: "must-have-one".into(),
                    expected: "1".into(),
                    got: args.len(),
                });
            }
            Ok(Value::Unspecified)
        });
        let r = p.call(&[]);
        match r {
            Err(FfiError::ArityError { name, .. }) => {
                assert_eq!(name, "must-have-one");
            }
            _ => panic!("expected ArityError"),
        }
    }

    /// Silence the test runner's default panic-printing hook while
    /// `f` runs, then restore it. The host-procedure panic-catch
    /// tests intentionally trigger panics; the default hook prints
    /// noisy backtraces to stderr that on macOS can collide with
    /// the test framework's own panic infrastructure ("failed to
    /// initiate panic, error 5"). Silencing fixes both.
    fn silence_panic<R>(f: impl FnOnce() -> R + std::panic::UnwindSafe) -> R {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = f();
        std::panic::set_hook(prev);
        r
    }

    #[test]
    fn untyped_proc_catches_panic() {
        let r = silence_panic(|| {
            let p = UntypedProc::new("boom", |_| {
                panic!("kaboom");
            });
            p.call(&[])
        });
        match r {
            Err(FfiError::Panic(msg)) => {
                assert!(msg.contains("kaboom"), "msg was: {}", msg);
            }
            other => panic!("expected Panic, got {:?}", other),
        }
    }

    #[test]
    fn untyped_proc_catches_string_panic() {
        let r = silence_panic(|| {
            let p = UntypedProc::new("boom2", |_| {
                panic!("{}", "string-payload-panic".to_string());
            });
            p.call(&[])
        });
        match r {
            Err(FfiError::Panic(msg)) => {
                assert!(msg.contains("string-payload-panic"));
            }
            _ => panic!("expected Panic"),
        }
    }
}
