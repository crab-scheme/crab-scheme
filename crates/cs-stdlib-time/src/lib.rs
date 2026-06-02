//! CrabScheme stdlib module: `(crab time)`.
//!
//! Wall-clock and monotonic time, sleep, strftime-style formatting.
//! Iter 5 of the `stdlib-modules` spec.
//!
//! Until the FFI layer gains an opaque-payload Scheme value, time
//! values are represented as fixnums (epoch seconds for wall, nanos
//! since process start for monotonic) instead of typed Instant /
//! Duration handles. `format-time` and `parse-time` work in epoch
//! seconds; `time-add` / `time-diff` are trivially `+` / `-` of two
//! fixnums and aren't bound as builtins for that reason.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `current-time`       | —                  | fixnum  | Unix epoch seconds. |
//! | `current-time-ms`    | —                  | fixnum  | Unix epoch milliseconds. |
//! | `monotonic-time-ns`  | —                  | fixnum  | Nanoseconds since process start. Strictly increases. |
//! | `sleep-ms`           | fixnum             | unspec  | Block current thread N ms. |
//! | `format-time`        | fixnum string      | string  | strftime; arg 1 is epoch seconds. |
//! | `parse-time`         | string string      | fixnum or #f | strptime; returns epoch seconds. |

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Timelike, Utc};
use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("current-time", current_time),
        UntypedProc::new("current-time-ms", current_time_ms),
        UntypedProc::new("monotonic-time-ns", monotonic_time_ns),
        UntypedProc::new("sleep-ms", sleep_ms),
        UntypedProc::new("format-time", format_time),
        UntypedProc::new("parse-time", parse_time),
        UntypedProc::new("time-year", time_year),
        UntypedProc::new("time-month", time_month),
        UntypedProc::new("time-day", time_day),
        UntypedProc::new("time-hour", time_hour),
        UntypedProc::new("time-minute", time_minute),
        UntypedProc::new("time-second", time_second),
        UntypedProc::new("time-weekday", time_weekday),
        UntypedProc::new("time-make", time_make),
        UntypedProc::new("time-add-days", time_add_days),
        UntypedProc::new("time-leap-year?", time_leap_year_p),
        UntypedProc::new("time-days-in-month", time_days_in_month),
        UntypedProc::new("time-day-of-year", time_day_of_year),
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

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Number(cs_core::Number::Fixnum(v))) => Ok(*v),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum".into(),
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
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
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

// Process-start instant; lazily initialized on first call. Subsequent
// calls measure elapsed nanos against this baseline so the value
// strictly increases across the process lifetime.
fn process_start() -> Instant {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

// ----- wall clock -----

fn current_time(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("current-time", "0", args.len()));
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FfiError::HostFailure(format!("current-time: {}", e)))?
        .as_secs() as i64;
    Ok(Value::fixnum(secs))
}

fn current_time_ms(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("current-time-ms", "0", args.len()));
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FfiError::HostFailure(format!("current-time-ms: {}", e)))?
        .as_millis() as i64;
    Ok(Value::fixnum(ms))
}

// ----- monotonic -----

fn monotonic_time_ns(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("monotonic-time-ns", "0", args.len()));
    }
    let ns = process_start().elapsed().as_nanos() as i64;
    Ok(Value::fixnum(ns))
}

// ----- sleep -----

fn sleep_ms(args: &[Value]) -> Result<Value, FfiError> {
    let ms = expect_fixnum("sleep-ms", args, 0)?;
    if ms < 0 {
        return Err(FfiError::HostFailure("sleep-ms: negative duration".into()));
    }
    std::thread::sleep(Duration::from_millis(ms as u64));
    Ok(Value::Unspecified)
}

// ----- format / parse -----

fn format_time(args: &[Value]) -> Result<Value, FfiError> {
    let epoch_secs = expect_fixnum("format-time", args, 0)?;
    let fmt = expect_string("format-time", args, 1)?;
    let dt: DateTime<Utc> = match Utc.timestamp_opt(epoch_secs, 0).single() {
        Some(d) => d,
        None => {
            return Err(FfiError::HostFailure(format!(
                "format-time: epoch {} out of range",
                epoch_secs
            )));
        }
    };
    Ok(string_value(dt.format(&fmt).to_string()))
}

fn parse_time(args: &[Value]) -> Result<Value, FfiError> {
    let s = expect_string("parse-time", args, 0)?;
    let fmt = expect_string("parse-time", args, 1)?;
    match NaiveDateTime::parse_from_str(&s, &fmt) {
        Ok(naive) => Ok(Value::fixnum(naive.and_utc().timestamp())),
        Err(_) => Ok(Value::Boolean(false)),
    }
}

// ----- date components + arithmetic (UTC) -----

fn epoch_to_dt(name: &str, secs: i64) -> Result<DateTime<Utc>, FfiError> {
    Utc.timestamp_opt(secs, 0)
        .single()
        .ok_or_else(|| FfiError::HostFailure(format!("{}: epoch {} out of range", name, secs)))
}

/// Extract one UTC component from an epoch-seconds timestamp.
fn component(
    name: &str,
    args: &[Value],
    f: impl FnOnce(&DateTime<Utc>) -> i64,
) -> Result<Value, FfiError> {
    let secs = expect_fixnum(name, args, 0)?;
    Ok(Value::fixnum(f(&epoch_to_dt(name, secs)?)))
}

fn time_year(args: &[Value]) -> Result<Value, FfiError> {
    component("time-year", args, |d| d.year() as i64)
}
fn time_month(args: &[Value]) -> Result<Value, FfiError> {
    component("time-month", args, |d| d.month() as i64)
}
fn time_day(args: &[Value]) -> Result<Value, FfiError> {
    component("time-day", args, |d| d.day() as i64)
}
fn time_hour(args: &[Value]) -> Result<Value, FfiError> {
    component("time-hour", args, |d| d.hour() as i64)
}
fn time_minute(args: &[Value]) -> Result<Value, FfiError> {
    component("time-minute", args, |d| d.minute() as i64)
}
fn time_second(args: &[Value]) -> Result<Value, FfiError> {
    component("time-second", args, |d| d.second() as i64)
}
/// Day of week as 0 = Sunday .. 6 = Saturday (matches C `tm_wday`).
fn time_weekday(args: &[Value]) -> Result<Value, FfiError> {
    component("time-weekday", args, |d| {
        d.weekday().num_days_from_sunday() as i64
    })
}

/// `(time-make year month day hour minute second)` → epoch seconds (UTC).
fn time_make(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 6 {
        return Err(arity("time-make", "6", args.len()));
    }
    let y = expect_fixnum("time-make", args, 0)? as i32;
    let mo = expect_fixnum("time-make", args, 1)? as u32;
    let d = expect_fixnum("time-make", args, 2)? as u32;
    let h = expect_fixnum("time-make", args, 3)? as u32;
    let mi = expect_fixnum("time-make", args, 4)? as u32;
    let se = expect_fixnum("time-make", args, 5)? as u32;
    match Utc.with_ymd_and_hms(y, mo, d, h, mi, se).single() {
        Some(dt) => Ok(Value::fixnum(dt.timestamp())),
        None => Err(FfiError::HostFailure(format!(
            "time-make: invalid date/time {}-{:02}-{:02} {:02}:{:02}:{:02}",
            y, mo, d, h, mi, se
        ))),
    }
}

/// `(time-add-days ts n)` → ts advanced by `n` whole days (n may be negative).
fn time_add_days(args: &[Value]) -> Result<Value, FfiError> {
    let secs = expect_fixnum("time-add-days", args, 0)?;
    let days = expect_fixnum("time-add-days", args, 1)?;
    Ok(Value::fixnum(secs + days * 86_400))
}

// ----- calendar helpers -----

fn is_leap(y: i64) -> bool {
    (y % 4 == 0) && (y % 100 != 0 || y % 400 == 0)
}

/// `(time-leap-year? year)` → whether `year` is a Gregorian leap year.
fn time_leap_year_p(args: &[Value]) -> Result<Value, FfiError> {
    Ok(Value::Boolean(is_leap(expect_fixnum(
        "time-leap-year?",
        args,
        0,
    )?)))
}

/// `(time-days-in-month year month)` → days in `month` (1-12) of `year`.
fn time_days_in_month(args: &[Value]) -> Result<Value, FfiError> {
    let y = expect_fixnum("time-days-in-month", args, 0)?;
    let m = expect_fixnum("time-days-in-month", args, 1)?;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        other => {
            return Err(FfiError::HostFailure(format!(
                "time-days-in-month: month must be 1-12, got {}",
                other
            )))
        }
    };
    Ok(Value::fixnum(days))
}

/// `(time-day-of-year ts)` → day of the year (1-366) for an epoch timestamp.
fn time_day_of_year(args: &[Value]) -> Result<Value, FfiError> {
    let secs = expect_fixnum("time-day-of-year", args, 0)?;
    Ok(Value::fixnum(
        epoch_to_dt("time-day-of-year", secs)?.ordinal() as i64,
    ))
}
