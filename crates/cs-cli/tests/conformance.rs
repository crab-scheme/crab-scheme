//! Conformance test harness.
//!
//! Loads every `.scm` file under `tests/conformance/foundation/` (except the
//! `_prelude.scm`) into a fresh Runtime alongside the prelude, then reads the
//! `__test-summary__` binding to get pass/fail counts.

use std::fs;
use std::path::Path;

use cs_core::{Value, WriteMode};
use cs_runtime::Runtime;

fn workspace_root() -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    format!("{}/../..", manifest)
}

fn load_test(path: &Path) -> (u64, u64, String) {
    let prelude_path = format!(
        "{}/tests/conformance/foundation/_prelude.scm",
        workspace_root()
    );
    let prelude = fs::read_to_string(&prelude_path).expect("read prelude");
    let body = fs::read_to_string(path).expect("read test file");

    let mut rt = Runtime::new();
    rt.eval_str("_prelude.scm", &prelude)
        .expect("prelude must evaluate");
    rt.eval_str(path.to_str().unwrap(), &body)
        .expect("test body must evaluate");
    let summary = rt
        .eval_str("<harness>", "(__test-summary__)")
        .expect("summary call");

    let (pass, fail, failures_str) = parse_summary(&rt, &summary);
    (pass, fail, failures_str)
}

fn parse_summary(rt: &Runtime, v: &Value) -> (u64, u64, String) {
    let mut iter = list_iter(v);
    let pass = iter
        .next()
        .and_then(|v| match v {
            Value::Number(n) => n.to_f64().to_u64(),
            _ => None,
        })
        .unwrap_or(0);
    let fail = iter
        .next()
        .and_then(|v| match v {
            Value::Number(n) => n.to_f64().to_u64(),
            _ => None,
        })
        .unwrap_or(0);
    let failures = iter.next().unwrap_or(Value::Null);
    let s = rt.format_value(&failures, WriteMode::Write);
    (pass, fail, s)
}

trait ToU64 {
    fn to_u64(self) -> Option<u64>;
}
impl ToU64 for f64 {
    fn to_u64(self) -> Option<u64> {
        if self.is_finite() && self >= 0.0 && self <= u64::MAX as f64 {
            Some(self as u64)
        } else {
            None
        }
    }
}

fn list_iter(v: &Value) -> ListIter {
    ListIter { cur: v.clone() }
}

struct ListIter {
    cur: Value,
}
impl Iterator for ListIter {
    type Item = Value;
    fn next(&mut self) -> Option<Value> {
        match &self.cur.clone() {
            Value::Pair(p) => {
                let car = p.car.borrow().clone();
                let cdr = p.cdr.borrow().clone();
                self.cur = cdr;
                Some(car)
            }
            _ => None,
        }
    }
}

fn run_conformance_file(name: &str) {
    let path = format!("{}/tests/conformance/foundation/{}", workspace_root(), name);
    let (pass, fail, failures) = load_test(Path::new(&path));
    println!("[{}] pass={} fail={}", name, pass, fail);
    if fail > 0 {
        println!("failures: {}", failures);
    }
    assert_eq!(fail, 0, "{}: {} test(s) failed: {}", name, fail, failures);
    assert!(pass > 0, "{}: no tests ran", name);
}

#[test]
fn conformance_arithmetic() {
    run_conformance_file("arithmetic.scm");
}

#[test]
fn conformance_lists() {
    run_conformance_file("lists.scm");
}

#[test]
fn conformance_equality() {
    run_conformance_file("equality.scm");
}

#[test]
fn conformance_control() {
    run_conformance_file("control.scm");
}

#[test]
fn conformance_strings() {
    run_conformance_file("strings.scm");
}

#[test]
fn conformance_characters() {
    run_conformance_file("characters.scm");
}

#[test]
fn conformance_vectors() {
    run_conformance_file("vectors.scm");
}

#[test]
fn conformance_booleans() {
    run_conformance_file("booleans.scm");
}

#[test]
fn conformance_case_and_assoc() {
    run_conformance_file("case_and_assoc.scm");
}

#[test]
fn conformance_exceptions() {
    run_conformance_file("exceptions.scm");
}

#[test]
fn conformance_string_ops() {
    run_conformance_file("string_ops.scm");
}

#[test]
fn conformance_numeric_extras() {
    run_conformance_file("numeric_extras.scm");
}

#[test]
fn conformance_char_extras() {
    run_conformance_file("char_extras.scm");
}

#[test]
fn conformance_do_and_guard() {
    run_conformance_file("do_and_guard.scm");
}

#[test]
fn conformance_hashtables() {
    run_conformance_file("hashtables.scm");
}

#[test]
fn conformance_multi_values() {
    run_conformance_file("multi_values.scm");
}

#[test]
fn conformance_srfi1_ops() {
    run_conformance_file("srfi1_ops.scm");
}

#[test]
fn conformance_records() {
    run_conformance_file("records.scm");
}

#[test]
fn conformance_ports() {
    run_conformance_file("ports.scm");
}

#[test]
fn conformance_promises() {
    run_conformance_file("promises.scm");
}

#[test]
fn conformance_let_values_dynwind() {
    run_conformance_file("let_values_dynwind.scm");
}

#[test]
fn conformance_bytevectors_misc() {
    run_conformance_file("bytevectors_misc.scm");
}

#[test]
fn conformance_bitwise_eval() {
    run_conformance_file("bitwise_eval.scm");
}

#[test]
fn conformance_strings_vectors_io() {
    run_conformance_file("strings_vectors_io.scm");
}

#[test]
fn conformance_parameters_srfi1() {
    run_conformance_file("parameters_srfi1.scm");
}

#[test]
fn conformance_quasiquote() {
    run_conformance_file("quasiquote.scm");
}

#[test]
fn conformance_transcendental_io() {
    run_conformance_file("transcendental_io.scm");
}

#[test]
fn conformance_sorting_files() {
    run_conformance_file("sorting_files.scm");
}

#[test]
fn conformance_copy_unfold_htfold() {
    run_conformance_file("copy_unfold_htfold.scm");
}

#[test]
fn conformance_conditions_r6rs() {
    run_conformance_file("conditions_r6rs.scm");
}

#[test]
fn conformance_macros() {
    run_conformance_file("macros.scm");
}

#[test]
fn conformance_macros2() {
    run_conformance_file("macros2.scm");
}

#[test]
fn conformance_macro_hygiene() {
    run_conformance_file("macro_hygiene.scm");
}

/// Aggregate count over a small subset to keep the test thread stack happy.
/// Per-file tests above already exercise everything; this just gates the
/// total pass count for visibility.
#[test]
fn conformance_aggregate_count() {
    // Spawn a thread with a larger stack since the tree-walker recurses on the
    // host stack inside higher-order builtins.
    let handle = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let files = [
                "arithmetic.scm",
                "lists.scm",
                "equality.scm",
                "control.scm",
                "strings.scm",
                "characters.scm",
                "vectors.scm",
                "booleans.scm",
                "case_and_assoc.scm",
                "exceptions.scm",
                "string_ops.scm",
                "numeric_extras.scm",
                "char_extras.scm",
                "do_and_guard.scm",
                "hashtables.scm",
                "multi_values.scm",
                "srfi1_ops.scm",
                "records.scm",
                "ports.scm",
                "promises.scm",
                "let_values_dynwind.scm",
                "bytevectors_misc.scm",
                "bitwise_eval.scm",
                "strings_vectors_io.scm",
                "parameters_srfi1.scm",
                "quasiquote.scm",
                "transcendental_io.scm",
                "sorting_files.scm",
                "copy_unfold_htfold.scm",
                "conditions_r6rs.scm",
                "macros.scm",
                "macros2.scm",
                "macro_hygiene.scm",
            ];
            let mut total_pass = 0u64;
            for f in files {
                let path = format!("{}/tests/conformance/foundation/{}", workspace_root(), f);
                let (p, _, _) = load_test(Path::new(&path));
                total_pass += p;
                println!("{} pass count = {}, running total = {}", f, p, total_pass);
            }
            total_pass
        })
        .expect("spawn aggregate thread");
    let total_pass = handle.join().expect("aggregate thread panicked");
    println!("total conformance pass count: {}", total_pass);
    assert!(
        total_pass >= 630,
        "expected ≥630 passing conformance tests, got {}",
        total_pass
    );
}
