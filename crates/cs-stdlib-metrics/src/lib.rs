//! CrabScheme stdlib module: `(crab metrics)`.
//!
//! In-process counters, gauges, and histograms. Iter 8 of the
//! `stdlib-modules` spec.
//!
//! Self-contained — no dep on the `metrics` ecosystem crate. The
//! shape mirrors Prometheus / OpenTelemetry vocabulary so callers
//! can shovel `(metrics-snapshot)` output into either system later.
//! Counters are monotonic u64; gauges are i64; histograms record
//! every observation into a Vec for now (post-iter-8 may swap in a
//! sparse / log-bucket representation).
//!
//! Metric names are interned in a registry keyed by string. Reading
//! `(counter-increment! "name" delta)` lazily creates the counter.
//! Conflicting types raise — `counter-increment!` on a name
//! registered as a gauge errors.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `counter-increment!`  | name [delta=1]    | unspec | Lazy create. |
//! | `counter-value`       | name              | fixnum | 0 if missing. |
//! | `gauge-set!`          | name value        | unspec | Lazy create. |
//! | `gauge-value`         | name              | fixnum | 0 if missing. |
//! | `histogram-observe!`  | name value        | unspec | Lazy create. |
//! | `histogram-summary`   | name              | alist  | (("count" . N) ("min" . v) ("p50" . v) ("p95" . v) ("p99" . v) ("max" . v) ("sum" . v)) |
//! | `metrics-snapshot`    | —                 | alist  | (("name" . kind) …) over every registered metric. |
//! | `metrics-reset!`      | —                 | unspec | Drop the entire registry. Useful in tests. |

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

#[derive(Debug)]
enum Metric {
    Counter(u64),
    Gauge(i64),
    Histogram(Vec<f64>),
}

fn registry() -> &'static Mutex<HashMap<String, Metric>> {
    static R: OnceLock<Mutex<HashMap<String, Metric>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("counter-increment!", counter_increment),
        UntypedProc::new("counter-value", counter_value),
        UntypedProc::new("gauge-set!", gauge_set),
        UntypedProc::new("gauge-value", gauge_value),
        UntypedProc::new("histogram-observe!", histogram_observe),
        UntypedProc::new("histogram-summary", histogram_summary),
        UntypedProc::new("metrics-snapshot", metrics_snapshot),
        UntypedProc::new("metrics-reset!", metrics_reset),
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
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_fixnum_opt(args: &[Value], idx: usize, default: i64) -> Result<i64, FfiError> {
    match args.get(idx) {
        None => Ok(default),
        Some(Value::Fixnum(v)) => Ok(*v),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum or no arg",
            got: other.type_name().to_string(),
        }),
    }
}

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Fixnum(v)) => Ok(*v),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
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

fn string_value(s: impl Into<String>) -> Value {
    Value::string(s)
}

fn locked() -> Result<std::sync::MutexGuard<'static, HashMap<String, Metric>>, FfiError> {
    registry()
        .lock()
        .map_err(|e| FfiError::HostFailure(format!("metrics: registry poisoned: {}", e)))
}

fn type_clash(name: &str, registered_kind: &str, attempted: &str) -> FfiError {
    FfiError::HostFailure(format!(
        "{}: metric {:?} already registered as {}",
        attempted, name, registered_kind
    ))
}

// ----- procedures -----

fn counter_increment(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("counter-increment!", args, 0)?;
    let delta = expect_fixnum_opt(args, 1, 1)?;
    if delta < 0 {
        return Err(FfiError::HostFailure(
            "counter-increment!: negative delta".into(),
        ));
    }
    let mut r = locked()?;
    let entry = r.entry(name.clone()).or_insert(Metric::Counter(0));
    match entry {
        Metric::Counter(v) => *v = v.saturating_add(delta as u64),
        Metric::Gauge(_) => return Err(type_clash(&name, "gauge", "counter-increment!")),
        Metric::Histogram(_) => return Err(type_clash(&name, "histogram", "counter-increment!")),
    }
    Ok(Value::Unspecified)
}

fn counter_value(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("counter-value", args, 0)?;
    let r = locked()?;
    Ok(Value::fixnum(match r.get(&name) {
        Some(Metric::Counter(v)) => *v as i64,
        Some(Metric::Gauge(_)) => return Err(type_clash(&name, "gauge", "counter-value")),
        Some(Metric::Histogram(_)) => return Err(type_clash(&name, "histogram", "counter-value")),
        None => 0,
    }))
}

fn gauge_set(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("gauge-set!", args, 0)?;
    let value = expect_fixnum("gauge-set!", args, 1)?;
    let mut r = locked()?;
    let entry = r.entry(name.clone()).or_insert(Metric::Gauge(0));
    match entry {
        Metric::Gauge(v) => *v = value,
        Metric::Counter(_) => return Err(type_clash(&name, "counter", "gauge-set!")),
        Metric::Histogram(_) => return Err(type_clash(&name, "histogram", "gauge-set!")),
    }
    Ok(Value::Unspecified)
}

fn gauge_value(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("gauge-value", args, 0)?;
    let r = locked()?;
    Ok(Value::fixnum(match r.get(&name) {
        Some(Metric::Gauge(v)) => *v,
        Some(Metric::Counter(_)) => return Err(type_clash(&name, "counter", "gauge-value")),
        Some(Metric::Histogram(_)) => return Err(type_clash(&name, "histogram", "gauge-value")),
        None => 0,
    }))
}

fn histogram_observe(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("histogram-observe!", args, 0)?;
    let value = expect_number("histogram-observe!", args, 1)?;
    let mut r = locked()?;
    let entry = r
        .entry(name.clone())
        .or_insert(Metric::Histogram(Vec::new()));
    match entry {
        Metric::Histogram(v) => v.push(value),
        Metric::Counter(_) => return Err(type_clash(&name, "counter", "histogram-observe!")),
        Metric::Gauge(_) => return Err(type_clash(&name, "gauge", "histogram-observe!")),
    }
    Ok(Value::Unspecified)
}

fn histogram_summary(args: &[Value]) -> Result<Value, FfiError> {
    let name = expect_string("histogram-summary", args, 0)?;
    let r = locked()?;
    let samples = match r.get(&name) {
        Some(Metric::Histogram(v)) => v.clone(),
        Some(Metric::Counter(_)) => return Err(type_clash(&name, "counter", "histogram-summary")),
        Some(Metric::Gauge(_)) => return Err(type_clash(&name, "gauge", "histogram-summary")),
        None => Vec::new(),
    };
    drop(r);
    summarize(&samples)
}

fn summarize(samples: &[f64]) -> Result<Value, FfiError> {
    let count = samples.len();
    if count == 0 {
        return Ok(Value::list(vec![
            pair("count", Value::fixnum(0)),
            pair("min", Value::flonum(0.0)),
            pair("p50", Value::flonum(0.0)),
            pair("p95", Value::flonum(0.0)),
            pair("p99", Value::flonum(0.0)),
            pair("max", Value::flonum(0.0)),
            pair("sum", Value::flonum(0.0)),
        ]));
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min = sorted[0];
    let max = sorted[count - 1];
    let sum: f64 = sorted.iter().sum();
    let pct = |p: f64| -> f64 {
        let idx = ((count as f64 - 1.0) * p).round() as usize;
        sorted[idx.min(count - 1)]
    };
    Ok(Value::list(vec![
        pair("count", Value::fixnum(count as i64)),
        pair("min", Value::flonum(min)),
        pair("p50", Value::flonum(pct(0.5))),
        pair("p95", Value::flonum(pct(0.95))),
        pair("p99", Value::flonum(pct(0.99))),
        pair("max", Value::flonum(max)),
        pair("sum", Value::flonum(sum)),
    ]))
}

fn pair(k: &str, v: Value) -> Value {
    Value::Pair(Pair::new(string_value(k.to_string()), v))
}

fn metrics_snapshot(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("metrics-snapshot", "0", args.len()));
    }
    let r = locked()?;
    let mut entries: Vec<Value> = r
        .iter()
        .map(|(name, metric)| {
            let kind = match metric {
                Metric::Counter(_) => "counter",
                Metric::Gauge(_) => "gauge",
                Metric::Histogram(_) => "histogram",
            };
            pair(name, string_value(kind))
        })
        .collect();
    // Stable ordering for predictable output.
    entries.sort_by(|a, b| match (a, b) {
        (Value::Pair(ap), Value::Pair(bp)) => match (ap.car(), bp.car()) {
            (Value::String(s1), Value::String(s2)) => s1.borrow().cmp(&s2.borrow()),
            _ => std::cmp::Ordering::Equal,
        },
        _ => std::cmp::Ordering::Equal,
    });
    Ok(Value::list(entries))
}

fn metrics_reset(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("metrics-reset!", "0", args.len()));
    }
    locked()?.clear();
    Ok(Value::Unspecified)
}
