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

// --- stdlib-modules (see .spec-workflow/specs/stdlib-modules) ---

#[test]
#[cfg(feature = "stdlib-path")]
fn conformance_crab_path() {
    run_conformance_file("crab-path.scm");
}

#[test]
#[cfg(feature = "stdlib-fs")]
fn conformance_crab_fs() {
    run_conformance_file("crab-fs.scm");
}

#[test]
#[cfg(feature = "stdlib-os")]
fn conformance_crab_os() {
    run_conformance_file("crab-os.scm");
}

#[test]
#[cfg(feature = "stdlib-process")]
fn conformance_crab_process() {
    run_conformance_file("crab-process.scm");
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
fn conformance_records_parent() {
    run_conformance_file("records_parent.scm");
}

#[test]
fn conformance_records_procedural() {
    run_conformance_file("records_procedural.scm");
}

#[test]
fn conformance_r6rs_misc() {
    run_conformance_file("r6rs_misc.scm");
}

#[test]
fn conformance_srfi1_more() {
    run_conformance_file("srfi1_more.scm");
}

#[test]
fn conformance_srfi1_ho_walker() {
    run_conformance_file("srfi1_ho_walker.scm");
}

#[test]
fn conformance_define_condition_type() {
    run_conformance_file("define_condition_type.scm");
}

#[test]
fn conformance_error_who() {
    run_conformance_file("error_who.scm");
}

#[test]
fn conformance_builtin_errors() {
    run_conformance_file("builtin_errors.scm");
}

#[test]
fn conformance_cond_guard_arrow() {
    run_conformance_file("cond_guard_arrow.scm");
}

#[test]
fn conformance_library_import() {
    run_conformance_file("library_import.scm");
}

#[test]
fn conformance_display_condition() {
    run_conformance_file("display_condition.scm");
}

#[test]
fn conformance_define_values() {
    run_conformance_file("define_values.scm");
}

#[test]
fn conformance_hash_functions() {
    run_conformance_file("hash_functions.scm");
}

#[test]
fn conformance_utf8_codec() {
    run_conformance_file("utf8_codec.scm");
}

#[test]
fn conformance_eval_environment() {
    run_conformance_file("eval_environment.scm");
}

#[test]
fn conformance_bytevector_ports() {
    run_conformance_file("bytevector_ports.scm");
}

#[test]
fn conformance_r6rs_division() {
    run_conformance_file("r6rs_division.scm");
}

#[test]
fn conformance_cxxr_accessors() {
    run_conformance_file("cxxr_accessors.scm");
}

#[test]
fn conformance_srfi1_extras() {
    run_conformance_file("srfi1_extras.scm");
}

#[test]
fn conformance_string_extras() {
    run_conformance_file("string_extras.scm");
}

#[test]
fn conformance_numeric_char_predicates() {
    run_conformance_file("numeric_char_predicates.scm");
}

#[test]
fn conformance_ieee_literals() {
    run_conformance_file("ieee_literals.scm");
}

#[test]
fn conformance_bigint_expt() {
    run_conformance_file("bigint_expt.scm");
}

#[test]
fn conformance_bigint_division() {
    run_conformance_file("bigint_division.scm");
}

#[test]
fn conformance_bigint_div0_mod0() {
    run_conformance_file("bigint_div0_mod0.scm");
}

#[test]
fn conformance_fx_fl_ops() {
    run_conformance_file("fx_fl_ops.scm");
}

#[test]
fn conformance_exactness_extras() {
    run_conformance_file("exactness_extras.scm");
}

#[test]
fn conformance_bytevector_typed() {
    run_conformance_file("bytevector_typed.scm");
}

#[test]
fn conformance_ho_dispatch_hoist() {
    run_conformance_file("ho_dispatch_hoist.scm");
}

#[test]
fn conformance_endianness_macro() {
    run_conformance_file("endianness_macro.scm");
}

#[test]
fn conformance_string_search() {
    run_conformance_file("string_search.scm");
}

#[test]
fn conformance_r7rs_time_env() {
    run_conformance_file("r7rs_time_env.scm");
}

#[test]
fn conformance_hashtable_rich() {
    run_conformance_file("hashtable_rich.scm");
}

#[test]
fn conformance_r7rs_division() {
    run_conformance_file("r7rs_division.scm");
}

#[test]
fn conformance_vec_list_extras() {
    run_conformance_file("vec_list_extras.scm");
}

#[test]
fn conformance_hashtable_custom() {
    run_conformance_file("hashtable_custom.scm");
}

#[test]
fn conformance_library_imports() {
    run_conformance_file("library_imports.scm");
}

#[test]
fn conformance_library_validate() {
    run_conformance_file("library_validate.scm");
}

#[test]
fn conformance_r7rs_define_library() {
    run_conformance_file("r7rs_define_library.scm");
}

#[test]
fn conformance_exact_integer_sqrt() {
    run_conformance_file("exact_integer_sqrt.scm");
}

#[test]
fn conformance_r7rs_assoc_member_compare() {
    run_conformance_file("r7rs_assoc_member_compare.scm");
}

#[test]
fn conformance_r7rs_define_record_type() {
    run_conformance_file("r7rs_define_record_type.scm");
}

#[test]
fn conformance_r7rs_case_arrow() {
    run_conformance_file("r7rs_case_arrow.scm");
}

#[test]
fn conformance_r7rs_port_reads() {
    run_conformance_file("r7rs_port_reads.scm");
}

#[test]
fn conformance_r7rs_bytevector_literal() {
    run_conformance_file("r7rs_bytevector_literal.scm");
}

#[test]
fn conformance_r7rs_char_names() {
    run_conformance_file("r7rs_char_names.scm");
}

#[test]
fn conformance_r7rs_string_escapes() {
    run_conformance_file("r7rs_string_escapes.scm");
}

#[test]
fn conformance_r7rs_pipe_identifiers() {
    run_conformance_file("r7rs_pipe_identifiers.scm");
}

#[test]
fn conformance_r7rs_port_writes() {
    run_conformance_file("r7rs_port_writes.scm");
}

#[test]
fn conformance_r7rs_error_predicates() {
    run_conformance_file("r7rs_error_predicates.scm");
}

#[test]
fn conformance_r7rs_read_error() {
    run_conformance_file("r7rs_read_error.scm");
}

#[test]
fn conformance_r7rs_exit() {
    run_conformance_file("r7rs_exit.scm");
}

#[test]
fn conformance_r7rs_call_with_port() {
    run_conformance_file("r7rs_call_with_port.scm");
}

#[test]
fn conformance_r7rs_port_mgmt() {
    run_conformance_file("r7rs_port_mgmt.scm");
}

#[test]
fn conformance_r7rs_eq_predicates() {
    run_conformance_file("r7rs_eq_predicates.scm");
}

#[test]
fn conformance_r7rs_fill() {
    run_conformance_file("r7rs_fill.scm");
}

#[test]
fn conformance_r7rs_delay_force() {
    run_conformance_file("r7rs_delay_force.scm");
}

#[test]
fn conformance_r7rs_syntax_error() {
    run_conformance_file("r7rs_syntax_error.scm");
}

#[test]
fn conformance_r7rs_copy() {
    run_conformance_file("r7rs_copy.scm");
}

#[test]
fn conformance_r7rs_to_list() {
    run_conformance_file("r7rs_to_list.scm");
}

#[test]
fn conformance_r7rs_bytevector_fill() {
    run_conformance_file("r7rs_bytevector_fill.scm");
}

#[test]
fn conformance_r7rs_string_ctor() {
    run_conformance_file("r7rs_string_ctor.scm");
}

#[test]
fn conformance_r7rs_string_map() {
    run_conformance_file("r7rs_string_map.scm");
}

#[test]
fn conformance_r7rs_ci_compare() {
    run_conformance_file("r7rs_ci_compare.scm");
}

#[test]
fn conformance_r7rs_cond_expand_lib() {
    run_conformance_file("r7rs_cond_expand_lib.scm");
}

#[test]
fn conformance_cond_expand_assert() {
    // Added 2026-05-16 during M10 Track W closeout. The fixture
    // existed in `tests/conformance/foundation/` but was never wired
    // into the cargo test runner — that's why the W4 conformance
    // sweep surfaced a `cond-expand-library-false` failure that
    // `cargo test` was silent about. The fixture is also fixed to
    // assert both polarities of `(library ...)` against the
    // expander's actual `cond_expand_match` behavior (R7RS stdlib
    // names return #t; clearly-fake names return #f).
    run_conformance_file("cond_expand_assert.scm");
}

// ---------------------------------------------------------------------
// Post-M10 cleanup (2026-05-16): wire the 5 remaining unwired fixtures
// per the "always wire fixtures into the runner" lesson from the
// cond-expand-library silent-failure bug.
// ---------------------------------------------------------------------

#[test]
fn conformance_call_cc() {
    // Exercises call/cc escape continuations on the walker tier.
    // Independent of the M8 first-class call/cc work (that's about
    // the VM tier); the walker has always supported escape
    // continuations via direct host-stack unwind.
    run_conformance_file("call_cc.scm");
}

#[test]
fn conformance_case_lambda() {
    // case-lambda arity-dispatched procedures, including rest patterns.
    run_conformance_file("case_lambda.scm");
}

#[test]
fn conformance_jit_cross_lambda_loop() {
    // Regression test for the cross-lambda Fixnum-return loop bug
    // (docs/research/jit_loop_cross_lambda_bug.md). The fix landed
    // in M6 Phase 4 iter 3 via BoxTyped support in uniform-NB; this
    // fixture pins the test so re-introducing the bug fails CI.
    run_conformance_file("jit_cross_lambda_loop.scm");
}

#[test]
fn conformance_named_let_assert() {
    // Named-let smoke (sum / list-build / fact) + `assert` semantics
    // (truthy returns unspecified; falsy raises a condition).
    run_conformance_file("named_let_assert.scm");
}

#[test]
fn conformance_enumerations() {
    // (rnrs enums) — R6RS §13. Per the fixture header it was
    // expected to error pre-M9 iter 2, but landed alongside the
    // M9 foundation builtins. Wiring it in pins coverage.
    run_conformance_file("enumerations.scm");
}

#[test]
fn conformance_r7rs_bytevector_list() {
    run_conformance_file("r7rs_bytevector_list.scm");
}

#[test]
fn conformance_r7rs_read_default_port() {
    run_conformance_file("r7rs_read_default_port.scm");
}

#[test]
fn conformance_r7rs_write_default_port() {
    run_conformance_file("r7rs_write_default_port.scm");
}

#[test]
fn conformance_r7rs_binary_default_port() {
    run_conformance_file("r7rs_binary_default_port.scm");
}

#[test]
fn conformance_r7rs_current_error_port() {
    run_conformance_file("r7rs_current_error_port.scm");
}

#[test]
fn conformance_r7rs_write_aliases() {
    run_conformance_file("r7rs_write_aliases.scm");
}

#[test]
fn conformance_r7rs_string_to_number() {
    run_conformance_file("r7rs_string_to_number.scm");
}

#[test]
fn conformance_r7rs_number_to_string() {
    run_conformance_file("r7rs_number_to_string.scm");
}

#[test]
fn conformance_r7rs_environments() {
    run_conformance_file("r7rs_environments.scm");
}

#[test]
fn conformance_r7rs_read_bytevector_bang() {
    run_conformance_file("r7rs_read_bytevector_bang.scm");
}

#[test]
fn conformance_r7rs_load() {
    run_conformance_file("r7rs_load.scm");
}

#[test]
fn conformance_r7rs_numeric_predicates() {
    run_conformance_file("r7rs_numeric_predicates.scm");
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
                "records_parent.scm",
                "records_procedural.scm",
                "r6rs_misc.scm",
                "srfi1_more.scm",
                "srfi1_ho_walker.scm",
                "define_condition_type.scm",
                "error_who.scm",
                "builtin_errors.scm",
                "cond_guard_arrow.scm",
                "library_import.scm",
                "display_condition.scm",
                "define_values.scm",
                "hash_functions.scm",
                "utf8_codec.scm",
                "eval_environment.scm",
                "bytevector_ports.scm",
                "r6rs_division.scm",
                "cxxr_accessors.scm",
                "srfi1_extras.scm",
                "string_extras.scm",
                "numeric_char_predicates.scm",
                "ieee_literals.scm",
                "bigint_expt.scm",
                "bigint_division.scm",
                "bigint_div0_mod0.scm",
                "fx_fl_ops.scm",
                "exactness_extras.scm",
                "bytevector_typed.scm",
                "ho_dispatch_hoist.scm",
                "endianness_macro.scm",
                "string_search.scm",
                "r7rs_time_env.scm",
                "hashtable_rich.scm",
                "r7rs_division.scm",
                "vec_list_extras.scm",
                "hashtable_custom.scm",
                "library_imports.scm",
                "library_validate.scm",
                "r7rs_define_library.scm",
                "exact_integer_sqrt.scm",
                "r7rs_assoc_member_compare.scm",
                "r7rs_define_record_type.scm",
                "r7rs_case_arrow.scm",
                "r7rs_port_reads.scm",
                "r7rs_bytevector_literal.scm",
                "r7rs_char_names.scm",
                "r7rs_string_escapes.scm",
                "r7rs_pipe_identifiers.scm",
                "r7rs_port_writes.scm",
                "r7rs_error_predicates.scm",
                "r7rs_read_error.scm",
                "r7rs_exit.scm",
                "r7rs_call_with_port.scm",
                "r7rs_port_mgmt.scm",
                "r7rs_eq_predicates.scm",
                "r7rs_fill.scm",
                "r7rs_delay_force.scm",
                "r7rs_syntax_error.scm",
                "r7rs_copy.scm",
                "r7rs_to_list.scm",
                "r7rs_bytevector_fill.scm",
                "r7rs_string_ctor.scm",
                "r7rs_string_map.scm",
                "r7rs_ci_compare.scm",
                "r7rs_cond_expand_lib.scm",
                "r7rs_bytevector_list.scm",
                "r7rs_read_default_port.scm",
                "r7rs_write_default_port.scm",
                "r7rs_binary_default_port.scm",
                "r7rs_current_error_port.scm",
                "r7rs_write_aliases.scm",
                "r7rs_string_to_number.scm",
                "r7rs_number_to_string.scm",
                "r7rs_environments.scm",
                "r7rs_read_bytevector_bang.scm",
                "r7rs_load.scm",
                "r7rs_numeric_predicates.scm",
                "macros.scm",
                "macros2.scm",
                "macro_hygiene.scm",
                "call_cc.scm",
                "case_lambda.scm",
                "jit_cross_lambda_loop.scm",
                "named_let_assert.scm",
                "enumerations.scm",
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
