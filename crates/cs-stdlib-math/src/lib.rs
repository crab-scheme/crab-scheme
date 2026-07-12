//! CrabScheme stdlib module: `(crab math)` + `(crab math stats)`.
//!
//! Iter 12 of the `stdlib-modules` spec.
//!
//! R6RS already covers basic transcendentals (sin/cos/tan/log/exp/
//! sqrt) and the integer + rational tower. This module adds the
//! "everyone reaches for this eventually" extras:
//!
//! - `math-erf` / `math-erfc` (error function + complement)
//! - `math-gamma` / `math-lgamma` (gamma function + log gamma)
//! - `math-cbrt` (cube root, faster than `(expt x 1/3)`)
//! - `math-hypot` (Pythagorean, no overflow)
//!
//! Plus `(crab math stats)` — descriptive statistics over a list
//! of numbers:
//!
//! - `stats-mean`, `stats-median`, `stats-variance`,
//!   `stats-stddev`, `stats-percentile`
//!
//! Statistics procedures accept a Scheme list of numbers and
//! return flonums. Empty input raises.
//!
//! ## Registered procedures
//!
//! | Name | Args | Returns |
//! |---|---|---|
//! | `math-erf`       | flonum            | flonum |
//! | `math-erfc`      | flonum            | flonum |
//! | `math-gamma`     | flonum            | flonum |
//! | `math-lgamma`    | flonum            | flonum |
//! | `math-cbrt`      | flonum            | flonum |
//! | `math-hypot`     | flonum flonum     | flonum |
//! | `stats-mean`     | list-of-numbers   | flonum |
//! | `stats-median`   | list-of-numbers   | flonum |
//! | `stats-variance` | list-of-numbers   | flonum (sample variance, n-1 denom) |
//! | `stats-stddev`   | list-of-numbers   | flonum |
//! | `stats-percentile` | list-of-numbers fraction | flonum (linear interp; fraction in [0,1]) |

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("math-erf", math_erf),
        UntypedProc::new("math-erfc", math_erfc),
        UntypedProc::new("math-gamma", math_gamma),
        UntypedProc::new("math-lgamma", math_lgamma),
        UntypedProc::new("math-cbrt", math_cbrt),
        UntypedProc::new("math-hypot", math_hypot),
        UntypedProc::new("stats-mean", stats_mean),
        UntypedProc::new("stats-median", stats_median),
        UntypedProc::new("stats-variance", stats_variance),
        UntypedProc::new("stats-stddev", stats_stddev),
        UntypedProc::new("stats-percentile", stats_percentile),
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

fn expect_number(name: &str, args: &[Value], idx: usize) -> Result<f64, FfiError> {
    match args.get(idx) {
        Some(
            nv @ (Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)),
        ) => {
            let n = nv.as_number().unwrap();
            Ok(n.to_f64())
        }
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "number",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_list_of_numbers(name: &str, args: &[Value], idx: usize) -> Result<Vec<f64>, FfiError> {
    let mut cur =
        args.get(idx)
            .cloned()
            .ok_or(arity(name, &format!(">= {}", idx + 1), args.len()))?;
    let mut out = Vec::new();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                match p.car() {
                    nv @ (Value::Fixnum(_)
                    | Value::Flonum(_)
                    | Value::BigNumber(_)
                    | Value::Rational(_)) => {
                        let n = nv.as_number().unwrap();
                        out.push(n.to_f64())
                    }
                    other => {
                        return Err(FfiError::HostFailure(format!(
                            "{}: list contains non-number ({})",
                            name,
                            other.type_name()
                        )));
                    }
                }
                cur = p.cdr();
            }
            other => {
                return Err(FfiError::HostFailure(format!(
                    "{}: expected proper list of numbers, got {}",
                    name,
                    other.type_name()
                )));
            }
        }
    }
}

// ----- math extras -----

fn math_erf(args: &[Value]) -> Result<Value, FfiError> {
    let x = expect_number("math-erf", args, 0)?;
    Ok(Value::flonum(libm::erf(x)))
}

fn math_erfc(args: &[Value]) -> Result<Value, FfiError> {
    let x = expect_number("math-erfc", args, 0)?;
    Ok(Value::flonum(libm::erfc(x)))
}

fn math_gamma(args: &[Value]) -> Result<Value, FfiError> {
    let x = expect_number("math-gamma", args, 0)?;
    Ok(Value::flonum(libm::tgamma(x)))
}

fn math_lgamma(args: &[Value]) -> Result<Value, FfiError> {
    let x = expect_number("math-lgamma", args, 0)?;
    Ok(Value::flonum(libm::lgamma(x)))
}

fn math_cbrt(args: &[Value]) -> Result<Value, FfiError> {
    let x = expect_number("math-cbrt", args, 0)?;
    Ok(Value::flonum(libm::cbrt(x)))
}

fn math_hypot(args: &[Value]) -> Result<Value, FfiError> {
    let a = expect_number("math-hypot", args, 0)?;
    let b = expect_number("math-hypot", args, 1)?;
    Ok(Value::flonum(libm::hypot(a, b)))
}

// ----- stats -----

fn require_nonempty(name: &str, samples: &[f64]) -> Result<(), FfiError> {
    if samples.is_empty() {
        Err(FfiError::HostFailure(format!(
            "{}: empty sample list",
            name
        )))
    } else {
        Ok(())
    }
}

fn stats_mean(args: &[Value]) -> Result<Value, FfiError> {
    let xs = expect_list_of_numbers("stats-mean", args, 0)?;
    require_nonempty("stats-mean", &xs)?;
    let sum: f64 = xs.iter().sum();
    Ok(Value::flonum(sum / xs.len() as f64))
}

fn stats_median(args: &[Value]) -> Result<Value, FfiError> {
    let mut xs = expect_list_of_numbers("stats-median", args, 0)?;
    require_nonempty("stats-median", &xs)?;
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = xs.len() / 2;
    Ok(Value::flonum(if xs.len() % 2 == 0 {
        (xs[mid - 1] + xs[mid]) / 2.0
    } else {
        xs[mid]
    }))
}

fn stats_variance(args: &[Value]) -> Result<Value, FfiError> {
    let xs = expect_list_of_numbers("stats-variance", args, 0)?;
    require_nonempty("stats-variance", &xs)?;
    if xs.len() < 2 {
        return Ok(Value::flonum(0.0));
    }
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let ss: f64 = xs.iter().map(|x| (x - mean).powi(2)).sum();
    Ok(Value::flonum(ss / (xs.len() - 1) as f64))
}

fn stats_stddev(args: &[Value]) -> Result<Value, FfiError> {
    let xs = expect_list_of_numbers("stats-stddev", args, 0)?;
    require_nonempty("stats-stddev", &xs)?;
    if xs.len() < 2 {
        return Ok(Value::flonum(0.0));
    }
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let ss: f64 = xs.iter().map(|x| (x - mean).powi(2)).sum();
    Ok(Value::flonum((ss / (xs.len() - 1) as f64).sqrt()))
}

fn stats_percentile(args: &[Value]) -> Result<Value, FfiError> {
    let mut xs = expect_list_of_numbers("stats-percentile", args, 0)?;
    require_nonempty("stats-percentile", &xs)?;
    let p = expect_number("stats-percentile", args, 1)?;
    if !(0.0..=1.0).contains(&p) {
        return Err(FfiError::HostFailure(format!(
            "stats-percentile: fraction {} not in [0, 1]",
            p
        )));
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = p * (xs.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    Ok(Value::flonum(xs[lo] + frac * (xs[hi] - xs[lo])))
}
