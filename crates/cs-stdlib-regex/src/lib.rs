//! CrabScheme stdlib module: `(crab regex)`.
//!
//! Regular expression matching backed by the `regex` crate. Iter 4
//! of the `stdlib-modules` spec.
//!
//! Patterns are passed in as strings on every call. Internally a
//! per-process LRU cache of compiled `Regex` values memoizes the
//! last 64 distinct patterns so tight match loops don't pay the
//! compile cost on every iteration. When an opaque-payload Scheme
//! value lands, a typed `(compile-regex …)` returning a reusable
//! handle replaces this scheme.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `regex-match?`        | pat str            | boolean | Anchored anywhere; same as `regex.is_match`. |
//! | `regex-find`          | pat str            | string or #f | First match's matched text. |
//! | `regex-find-all`      | pat str            | list of strings |
//! | `regex-replace`       | pat str repl       | string  | First match only. |
//! | `regex-replace-all`   | pat str repl       | string  |
//! | `regex-split`         | pat str            | list of strings | Split `str` at each match. |

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use regex::Regex;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("regex-match?", regex_match_p),
        UntypedProc::new("regex-find", regex_find),
        UntypedProc::new("regex-find-all", regex_find_all),
        UntypedProc::new("regex-replace", regex_replace),
        UntypedProc::new("regex-replace-all", regex_replace_all),
        UntypedProc::new("regex-split", regex_split),
    ]
}

// ----- compiled-pattern cache -----

const CACHE_CAP: usize = 64;

struct RegexCache {
    // Insertion-ordered eviction. Vec of (pattern, regex); first
    // entry is the oldest.
    entries: Vec<(String, Arc<Regex>)>,
    // Patterns currently resident for O(1) lookup.
    index: HashMap<String, Arc<Regex>>,
}

impl RegexCache {
    fn new() -> Self {
        Self {
            entries: Vec::with_capacity(CACHE_CAP),
            index: HashMap::with_capacity(CACHE_CAP),
        }
    }

    fn get_or_compile(&mut self, pat: &str) -> Result<Arc<Regex>, FfiError> {
        if let Some(r) = self.index.get(pat) {
            return Ok(Arc::clone(r));
        }
        let compiled = Regex::new(pat).map_err(|e| {
            FfiError::HostFailure(format!("regex: invalid pattern `{}`: {}", pat, e))
        })?;
        let arc = Arc::new(compiled);
        if self.entries.len() == CACHE_CAP {
            let (old_pat, _) = self.entries.remove(0);
            self.index.remove(&old_pat);
        }
        self.entries.push((pat.to_string(), Arc::clone(&arc)));
        self.index.insert(pat.to_string(), Arc::clone(&arc));
        Ok(arc)
    }
}

fn cache() -> &'static Mutex<RegexCache> {
    static C: OnceLock<Mutex<RegexCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(RegexCache::new()))
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

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn compile(pat: &str) -> Result<Arc<Regex>, FfiError> {
    let mut c = cache()
        .lock()
        .map_err(|e| FfiError::HostFailure(format!("regex: cache poisoned: {}", e)))?;
    c.get_or_compile(pat)
}

// ----- procedures -----

fn regex_match_p(args: &[Value]) -> Result<Value, FfiError> {
    let pat = expect_string("regex-match?", args, 0)?;
    let s = expect_string("regex-match?", args, 1)?;
    Ok(Value::Boolean(compile(&pat)?.is_match(&s)))
}

fn regex_find(args: &[Value]) -> Result<Value, FfiError> {
    let pat = expect_string("regex-find", args, 0)?;
    let s = expect_string("regex-find", args, 1)?;
    Ok(compile(&pat)?.find(&s).map_or(Value::Boolean(false), |m| {
        string_value(m.as_str().to_string())
    }))
}

fn regex_find_all(args: &[Value]) -> Result<Value, FfiError> {
    let pat = expect_string("regex-find-all", args, 0)?;
    let s = expect_string("regex-find-all", args, 1)?;
    let matches: Vec<Value> = compile(&pat)?
        .find_iter(&s)
        .map(|m| string_value(m.as_str().to_string()))
        .collect();
    Ok(Value::list(matches))
}

fn regex_replace(args: &[Value]) -> Result<Value, FfiError> {
    let pat = expect_string("regex-replace", args, 0)?;
    let s = expect_string("regex-replace", args, 1)?;
    let repl = expect_string("regex-replace", args, 2)?;
    Ok(string_value(
        compile(&pat)?.replace(&s, repl.as_str()).into_owned(),
    ))
}

fn regex_replace_all(args: &[Value]) -> Result<Value, FfiError> {
    let pat = expect_string("regex-replace-all", args, 0)?;
    let s = expect_string("regex-replace-all", args, 1)?;
    let repl = expect_string("regex-replace-all", args, 2)?;
    Ok(string_value(
        compile(&pat)?.replace_all(&s, repl.as_str()).into_owned(),
    ))
}

fn regex_split(args: &[Value]) -> Result<Value, FfiError> {
    let pat = expect_string("regex-split", args, 0)?;
    let s = expect_string("regex-split", args, 1)?;
    let parts: Vec<Value> = compile(&pat)?
        .split(&s)
        .map(|p| string_value(p.to_string()))
        .collect();
    Ok(Value::list(parts))
}
