//! CrabScheme stdlib module: `(crab format)`.
//!
//! Common Lisp-style printf formatting. Iter 4 of the
//! `stdlib-modules` spec.
//!
//! ## Directives
//!
//! | Directive | Behavior |
//! |---|---|
//! | `~a` | Display the next arg (humane form — strings unquoted). |
//! | `~s` | Write the next arg (readable form — strings quoted). |
//! | `~d` | Display the next arg as a decimal integer. |
//! | `~x` | Display the next arg as a hex integer (lowercase). |
//! | `~X` | Display the next arg as a hex integer (uppercase). |
//! | `~%` | Insert a newline. |
//! | `~~` | Literal `~`. |
//!
//! Unrecognized directives raise `FfiError::HostFailure`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `format-string` | fmt args… | string | Pure substitution; no IO. |

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![UntypedProc::new("format-string", format_string)]
}

fn format_string(args: &[Value]) -> Result<Value, FfiError> {
    let fmt = match args.first() {
        Some(Value::String(s)) => s.borrow().clone(),
        Some(other) => {
            return Err(FfiError::TypeMismatch {
                expected: "string".into(),
                got: other.type_name().to_string(),
            })
        }
        None => {
            return Err(FfiError::ArityError {
                name: "format-string".into(),
                expected: "at least 1".into(),
                got: 0,
            })
        }
    };
    let rest = &args[1..];

    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    let mut idx = 0usize;

    while let Some(c) = chars.next() {
        if c != '~' {
            out.push(c);
            continue;
        }
        let directive = chars.next().ok_or_else(|| {
            FfiError::HostFailure("format-string: trailing `~` with no directive".into())
        })?;
        match directive {
            '~' => out.push('~'),
            '%' => out.push('\n'),
            'a' | 's' | 'd' | 'x' | 'X' => {
                let arg = rest.get(idx).ok_or_else(|| {
                    FfiError::HostFailure(format!(
                        "format-string: directive ~{} consumes arg {} but only {} supplied",
                        directive,
                        idx,
                        rest.len()
                    ))
                })?;
                idx += 1;
                render(directive, arg, &mut out)?;
            }
            other => {
                return Err(FfiError::HostFailure(format!(
                    "format-string: unknown directive ~{}",
                    other
                )));
            }
        }
    }

    if idx < rest.len() {
        return Err(FfiError::HostFailure(format!(
            "format-string: {} extra arg(s) past last directive",
            rest.len() - idx
        )));
    }

    Ok(Value::String(cs_core::Gc::new(std::cell::RefCell::new(
        out,
    ))))
}

fn render(directive: char, v: &Value, out: &mut String) -> Result<(), FfiError> {
    use std::fmt::Write;
    match (directive, v) {
        ('a', Value::String(s)) => out.push_str(&s.borrow()),
        ('s', Value::String(s)) => {
            // Crude write form — wrap in quotes; escape \ and ".
            out.push('"');
            for c in s.borrow().chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    other => out.push(other),
                }
            }
            out.push('"');
        }
        ('a' | 's', Value::Character(c)) => out.push(*c),
        ('a' | 's' | 'd', Value::Number(n)) => {
            // ~a/~s/~d all defer to Number's Display impl, which
            // preserves precision across Fixnum/Big/Rat/Flonum.
            // ~d on a Flonum or Rat formats it as-is rather than
            // truncating to i64 (which silently corrupts ≥ 2^53).
            let _ = write!(out, "{}", n);
        }
        ('x', Value::Number(cs_core::Number::Fixnum(v))) => {
            let _ = write!(out, "{:x}", v);
        }
        ('X', Value::Number(cs_core::Number::Fixnum(v))) => {
            let _ = write!(out, "{:X}", v);
        }
        ('x' | 'X', Value::Number(_)) => {
            return Err(FfiError::HostFailure(
                "format-string: ~x/~X only formats fixnums (bignum/rational/flonum not supported)"
                    .into(),
            ));
        }
        ('a' | 's', Value::Boolean(true)) => out.push_str("#t"),
        ('a' | 's', Value::Boolean(false)) => out.push_str("#f"),
        ('a' | 's', Value::Null) => out.push_str("()"),
        ('a' | 's', Value::Unspecified) => out.push_str("#!unspecified"),
        (d, other) => {
            return Err(FfiError::HostFailure(format!(
                "format-string: directive ~{} can't render {}",
                d,
                other.type_name()
            )));
        }
    }
    Ok(())
}
