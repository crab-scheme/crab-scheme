//! VM-tier conformance test: run a subset of conformance .scm files through
//! the bytecode VM and verify the same pass count as the tree-walker.
//!
//! Foundation: only files using pure builtins (no `with-exception-handler`,
//! `apply`, `map`, etc.) are runnable. Files exercising higher-order ops
//! are skipped at this milestone — they need a cross-tier bridge.

use std::fs;

use cs_core::{Value, WriteMode};
use cs_runtime::Runtime;

fn workspace_root() -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    format!("{}/../..", manifest)
}

fn pass_count_walker(file: &str) -> u64 {
    let prelude = fs::read_to_string(format!(
        "{}/tests/conformance/foundation/_prelude.scm",
        workspace_root()
    ))
    .unwrap();
    let body = fs::read_to_string(format!(
        "{}/tests/conformance/foundation/{}",
        workspace_root(),
        file
    ))
    .unwrap();
    let mut rt = Runtime::new();
    rt.eval_str("_prelude.scm", &prelude).expect("prelude");
    rt.eval_str(file, &body).expect("body");
    let summary = rt.eval_str("<harness>", "(__test-summary__)").unwrap();
    extract_pass_count(&rt, &summary)
}

fn pass_count_vm(file: &str) -> Result<u64, String> {
    let prelude = fs::read_to_string(format!(
        "{}/tests/conformance/foundation/_prelude.scm",
        workspace_root()
    ))
    .unwrap();
    let body = fs::read_to_string(format!(
        "{}/tests/conformance/foundation/{}",
        workspace_root(),
        file
    ))
    .unwrap();
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("_prelude.scm", &prelude)
        .map_err(|d| d.message.clone())?;
    rt.eval_str_via_vm(file, &body)
        .map_err(|d| d.message.clone())?;
    let summary = rt
        .eval_str_via_vm("<harness>", "(__test-summary__)")
        .map_err(|d| d.message.clone())?;
    Ok(extract_pass_count(&rt, &summary))
}

fn extract_pass_count(rt: &Runtime, v: &Value) -> u64 {
    let s = rt.format_value(v, WriteMode::Write);
    // Summary shape: "(N M (failures))". Parse leading number.
    let trimmed = s.trim_start_matches('(');
    let first = trimmed.split_whitespace().next().unwrap_or("0");
    first.parse().unwrap_or(0)
}

#[test]
fn vm_conformance_booleans() {
    let walker = pass_count_walker("booleans.scm");
    let vm = pass_count_vm("booleans.scm").expect("vm should run booleans.scm");
    println!("booleans: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_equality() {
    let walker = pass_count_walker("equality.scm");
    let vm = pass_count_vm("equality.scm").expect("vm should run equality.scm");
    println!("equality: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_arithmetic() {
    let walker = pass_count_walker("arithmetic.scm");
    let vm = pass_count_vm("arithmetic.scm").expect("vm should run arithmetic.scm");
    println!("arithmetic: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_numeric_extras() {
    let walker = pass_count_walker("numeric_extras.scm");
    let vm = pass_count_vm("numeric_extras.scm").expect("vm should run numeric_extras.scm");
    println!("numeric_extras: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_characters() {
    let walker = pass_count_walker("characters.scm");
    let vm = pass_count_vm("characters.scm").expect("vm should run characters.scm");
    println!("characters: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_char_extras() {
    let walker = pass_count_walker("char_extras.scm");
    let vm = pass_count_vm("char_extras.scm").expect("vm should run char_extras.scm");
    println!("char_extras: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_vectors() {
    let walker = pass_count_walker("vectors.scm");
    let vm = pass_count_vm("vectors.scm").expect("vm should run vectors.scm");
    println!("vectors: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_strings() {
    let walker = pass_count_walker("strings.scm");
    let vm = pass_count_vm("strings.scm").expect("vm should run strings.scm");
    println!("strings: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_string_ops() {
    let walker = pass_count_walker("string_ops.scm");
    let vm = pass_count_vm("string_ops.scm").expect("vm should run string_ops.scm");
    println!("string_ops: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

// bytevectors_misc.scm uses gensym + with-output-to-string (HO ops);
// skipped on VM until cross-tier bridge lands.

#[test]
fn vm_conformance_quasiquote() {
    let walker = pass_count_walker("quasiquote.scm");
    let vm = pass_count_vm("quasiquote.scm").expect("vm should run quasiquote.scm");
    println!("quasiquote: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_records() {
    let walker = pass_count_walker("records.scm");
    let vm = pass_count_vm("records.scm").expect("vm should run records.scm");
    println!("records: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_lists() {
    let walker = pass_count_walker("lists.scm");
    let vm = pass_count_vm("lists.scm").expect("vm should run lists.scm");
    println!("lists: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_case_and_assoc() {
    let walker = pass_count_walker("case_and_assoc.scm");
    let vm = pass_count_vm("case_and_assoc.scm").expect("vm should run case_and_assoc.scm");
    println!("case_and_assoc: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_control() {
    let walker = pass_count_walker("control.scm");
    let vm = pass_count_vm("control.scm").expect("vm should run control.scm");
    println!("control: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_srfi1_ops() {
    let walker = pass_count_walker("srfi1_ops.scm");
    let vm = pass_count_vm("srfi1_ops.scm").expect("vm should run srfi1_ops.scm");
    println!("srfi1_ops: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_strings_vectors_io() {
    let walker = pass_count_walker("strings_vectors_io.scm");
    let vm = pass_count_vm("strings_vectors_io.scm").expect("vm should run strings_vectors_io.scm");
    println!("strings_vectors_io: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_copy_unfold_htfold() {
    let walker = pass_count_walker("copy_unfold_htfold.scm");
    let vm = pass_count_vm("copy_unfold_htfold.scm").expect("vm should run copy_unfold_htfold.scm");
    println!("copy_unfold_htfold: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_sorting_files() {
    let walker = pass_count_walker("sorting_files.scm");
    let vm = pass_count_vm("sorting_files.scm").expect("vm should run sorting_files.scm");
    println!("sorting_files: walker={} vm={}", walker, vm);
    if walker != vm {
        let prelude = fs::read_to_string(format!(
            "{}/tests/conformance/foundation/_prelude.scm",
            workspace_root()
        ))
        .unwrap();
        let body = fs::read_to_string(format!(
            "{}/tests/conformance/foundation/sorting_files.scm",
            workspace_root()
        ))
        .unwrap();
        let mut rt = Runtime::new();
        rt.eval_str_via_vm("_prelude.scm", &prelude).unwrap();
        rt.eval_str_via_vm("sorting_files.scm", &body).unwrap();
        let s = rt
            .eval_str_via_vm("<harness>", "(__test-summary__)")
            .unwrap();
        println!("VM summary: {}", rt.format_value(&s, WriteMode::Write));
    }
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_macros() {
    let walker = pass_count_walker("macros.scm");
    let vm = pass_count_vm("macros.scm").expect("vm should run macros.scm");
    println!("macros: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_macros2() {
    let walker = pass_count_walker("macros2.scm");
    let vm = pass_count_vm("macros2.scm").expect("vm should run macros2.scm");
    println!("macros2: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_macro_hygiene() {
    let walker = pass_count_walker("macro_hygiene.scm");
    let vm = pass_count_vm("macro_hygiene.scm").expect("vm should run macro_hygiene.scm");
    println!("macro_hygiene: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_multi_values() {
    let walker = pass_count_walker("multi_values.scm");
    let vm = pass_count_vm("multi_values.scm").expect("vm should run multi_values.scm");
    println!("multi_values: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_exceptions() {
    let walker = pass_count_walker("exceptions.scm");
    let vm = pass_count_vm("exceptions.scm").expect("vm should run exceptions.scm");
    println!("exceptions: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_bitwise_eval() {
    let walker = pass_count_walker("bitwise_eval.scm");
    let vm = pass_count_vm("bitwise_eval.scm").expect("vm should run bitwise_eval.scm");
    println!("bitwise_eval: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_do_and_guard() {
    let walker = pass_count_walker("do_and_guard.scm");
    let vm = pass_count_vm("do_and_guard.scm").expect("vm should run do_and_guard.scm");
    println!("do_and_guard: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_ports() {
    let walker = pass_count_walker("ports.scm");
    let vm = pass_count_vm("ports.scm").expect("vm should run ports.scm");
    println!("ports: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_promises() {
    let walker = pass_count_walker("promises.scm");
    let vm = pass_count_vm("promises.scm").expect("vm should run promises.scm");
    println!("promises: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_let_values_dynwind() {
    let walker = pass_count_walker("let_values_dynwind.scm");
    let vm = pass_count_vm("let_values_dynwind.scm").expect("vm should run let_values_dynwind.scm");
    println!("let_values_dynwind: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_parameters_srfi1() {
    let walker = pass_count_walker("parameters_srfi1.scm");
    let vm = pass_count_vm("parameters_srfi1.scm").expect("vm should run parameters_srfi1.scm");
    println!("parameters_srfi1: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_transcendental_io() {
    let walker = pass_count_walker("transcendental_io.scm");
    let vm = pass_count_vm("transcendental_io.scm").expect("vm should run transcendental_io.scm");
    println!("transcendental_io: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_bytevectors_misc() {
    let walker = pass_count_walker("bytevectors_misc.scm");
    let vm = pass_count_vm("bytevectors_misc.scm").expect("vm should run bytevectors_misc.scm");
    println!("bytevectors_misc: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_call_cc() {
    let walker = pass_count_walker("call_cc.scm");
    let vm = pass_count_vm("call_cc.scm").expect("vm should run call_cc.scm");
    println!("call_cc: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_named_let_assert() {
    let walker = pass_count_walker("named_let_assert.scm");
    let vm = pass_count_vm("named_let_assert.scm").expect("vm should run named_let_assert.scm");
    println!("named_let_assert: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_case_lambda() {
    let walker = pass_count_walker("case_lambda.scm");
    let vm = pass_count_vm("case_lambda.scm").expect("vm should run case_lambda.scm");
    println!("case_lambda: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_cond_expand_assert() {
    let walker = pass_count_walker("cond_expand_assert.scm");
    let vm = pass_count_vm("cond_expand_assert.scm").expect("vm should run cond_expand_assert.scm");
    println!("cond_expand_assert: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_hashtables() {
    let walker = pass_count_walker("hashtables.scm");
    let vm = pass_count_vm("hashtables.scm").expect("vm should run hashtables.scm");
    println!("hashtables: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_conditions_r6rs() {
    let walker = pass_count_walker("conditions_r6rs.scm");
    let vm = pass_count_vm("conditions_r6rs.scm").expect("vm should run conditions_r6rs.scm");
    println!("conditions_r6rs: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_records_parent() {
    let walker = pass_count_walker("records_parent.scm");
    let vm = pass_count_vm("records_parent.scm").expect("vm should run records_parent.scm");
    println!("records_parent: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_define_condition_type() {
    let walker = pass_count_walker("define_condition_type.scm");
    let vm = pass_count_vm("define_condition_type.scm")
        .expect("vm should run define_condition_type.scm");
    println!("define_condition_type: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_error_who() {
    let walker = pass_count_walker("error_who.scm");
    let vm = pass_count_vm("error_who.scm").expect("vm should run error_who.scm");
    println!("error_who: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_builtin_errors() {
    let walker = pass_count_walker("builtin_errors.scm");
    let vm = pass_count_vm("builtin_errors.scm").expect("vm should run builtin_errors.scm");
    println!("builtin_errors: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_cond_guard_arrow() {
    let walker = pass_count_walker("cond_guard_arrow.scm");
    let vm = pass_count_vm("cond_guard_arrow.scm").expect("vm should run cond_guard_arrow.scm");
    println!("cond_guard_arrow: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_library_import() {
    let walker = pass_count_walker("library_import.scm");
    let vm = pass_count_vm("library_import.scm").expect("vm should run library_import.scm");
    println!("library_import: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_display_condition() {
    let walker = pass_count_walker("display_condition.scm");
    let vm = pass_count_vm("display_condition.scm").expect("vm should run display_condition.scm");
    println!("display_condition: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_define_values() {
    let walker = pass_count_walker("define_values.scm");
    let vm = pass_count_vm("define_values.scm").expect("vm should run define_values.scm");
    println!("define_values: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_hash_functions() {
    let walker = pass_count_walker("hash_functions.scm");
    let vm = pass_count_vm("hash_functions.scm").expect("vm should run hash_functions.scm");
    println!("hash_functions: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_utf8_codec() {
    let walker = pass_count_walker("utf8_codec.scm");
    let vm = pass_count_vm("utf8_codec.scm").expect("vm should run utf8_codec.scm");
    println!("utf8_codec: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_eval_environment() {
    let walker = pass_count_walker("eval_environment.scm");
    let vm = pass_count_vm("eval_environment.scm").expect("vm should run eval_environment.scm");
    println!("eval_environment: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_bytevector_ports() {
    let walker = pass_count_walker("bytevector_ports.scm");
    let vm = pass_count_vm("bytevector_ports.scm").expect("vm should run bytevector_ports.scm");
    println!("bytevector_ports: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r6rs_division() {
    let walker = pass_count_walker("r6rs_division.scm");
    let vm = pass_count_vm("r6rs_division.scm").expect("vm should run r6rs_division.scm");
    println!("r6rs_division: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_cxxr_accessors() {
    let walker = pass_count_walker("cxxr_accessors.scm");
    let vm = pass_count_vm("cxxr_accessors.scm").expect("vm should run cxxr_accessors.scm");
    println!("cxxr_accessors: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_string_extras() {
    let walker = pass_count_walker("string_extras.scm");
    let vm = pass_count_vm("string_extras.scm").expect("vm should run string_extras.scm");
    println!("string_extras: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_numeric_char_predicates() {
    let walker = pass_count_walker("numeric_char_predicates.scm");
    let vm = pass_count_vm("numeric_char_predicates.scm")
        .expect("vm should run numeric_char_predicates.scm");
    println!("numeric_char_predicates: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_ieee_literals() {
    let walker = pass_count_walker("ieee_literals.scm");
    let vm = pass_count_vm("ieee_literals.scm").expect("vm should run ieee_literals.scm");
    println!("ieee_literals: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_bigint_expt() {
    let walker = pass_count_walker("bigint_expt.scm");
    let vm = pass_count_vm("bigint_expt.scm").expect("vm should run bigint_expt.scm");
    println!("bigint_expt: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_bigint_division() {
    let walker = pass_count_walker("bigint_division.scm");
    let vm = pass_count_vm("bigint_division.scm").expect("vm should run bigint_division.scm");
    println!("bigint_division: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_bigint_div0_mod0() {
    let walker = pass_count_walker("bigint_div0_mod0.scm");
    let vm = pass_count_vm("bigint_div0_mod0.scm").expect("vm should run bigint_div0_mod0.scm");
    println!("bigint_div0_mod0: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_fx_fl_ops() {
    let walker = pass_count_walker("fx_fl_ops.scm");
    let vm = pass_count_vm("fx_fl_ops.scm").expect("vm should run fx_fl_ops.scm");
    println!("fx_fl_ops: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_exactness_extras() {
    let walker = pass_count_walker("exactness_extras.scm");
    let vm = pass_count_vm("exactness_extras.scm").expect("vm should run exactness_extras.scm");
    println!("exactness_extras: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_bytevector_typed() {
    let walker = pass_count_walker("bytevector_typed.scm");
    let vm = pass_count_vm("bytevector_typed.scm").expect("vm should run bytevector_typed.scm");
    println!("bytevector_typed: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_ho_dispatch_hoist() {
    let walker = pass_count_walker("ho_dispatch_hoist.scm");
    let vm = pass_count_vm("ho_dispatch_hoist.scm").expect("vm should run ho_dispatch_hoist.scm");
    println!("ho_dispatch_hoist: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_endianness_macro() {
    let walker = pass_count_walker("endianness_macro.scm");
    let vm = pass_count_vm("endianness_macro.scm").expect("vm should run endianness_macro.scm");
    println!("endianness_macro: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_string_search() {
    let walker = pass_count_walker("string_search.scm");
    let vm = pass_count_vm("string_search.scm").expect("vm should run string_search.scm");
    println!("string_search: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_time_env() {
    let walker = pass_count_walker("r7rs_time_env.scm");
    let vm = pass_count_vm("r7rs_time_env.scm").expect("vm should run r7rs_time_env.scm");
    println!("r7rs_time_env: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_hashtable_rich() {
    let walker = pass_count_walker("hashtable_rich.scm");
    let vm = pass_count_vm("hashtable_rich.scm").expect("vm should run hashtable_rich.scm");
    println!("hashtable_rich: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_division() {
    let walker = pass_count_walker("r7rs_division.scm");
    let vm = pass_count_vm("r7rs_division.scm").expect("vm should run r7rs_division.scm");
    println!("r7rs_division: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_vec_list_extras() {
    let walker = pass_count_walker("vec_list_extras.scm");
    let vm = pass_count_vm("vec_list_extras.scm").expect("vm should run vec_list_extras.scm");
    println!("vec_list_extras: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_hashtable_custom() {
    let walker = pass_count_walker("hashtable_custom.scm");
    let vm = pass_count_vm("hashtable_custom.scm").expect("vm should run hashtable_custom.scm");
    println!("hashtable_custom: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_library_imports() {
    let walker = pass_count_walker("library_imports.scm");
    let vm = pass_count_vm("library_imports.scm").expect("vm should run library_imports.scm");
    println!("library_imports: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_library_validate() {
    let walker = pass_count_walker("library_validate.scm");
    let vm = pass_count_vm("library_validate.scm").expect("vm should run library_validate.scm");
    println!("library_validate: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_define_library() {
    let walker = pass_count_walker("r7rs_define_library.scm");
    let vm =
        pass_count_vm("r7rs_define_library.scm").expect("vm should run r7rs_define_library.scm");
    println!("r7rs_define_library: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_exact_integer_sqrt() {
    let walker = pass_count_walker("exact_integer_sqrt.scm");
    let vm = pass_count_vm("exact_integer_sqrt.scm").expect("vm should run exact_integer_sqrt.scm");
    println!("exact_integer_sqrt: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_assoc_member_compare() {
    let walker = pass_count_walker("r7rs_assoc_member_compare.scm");
    let vm = pass_count_vm("r7rs_assoc_member_compare.scm")
        .expect("vm should run r7rs_assoc_member_compare.scm");
    println!("r7rs_assoc_member_compare: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_define_record_type() {
    let walker = pass_count_walker("r7rs_define_record_type.scm");
    let vm = pass_count_vm("r7rs_define_record_type.scm")
        .expect("vm should run r7rs_define_record_type.scm");
    println!("r7rs_define_record_type: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_case_arrow() {
    let walker = pass_count_walker("r7rs_case_arrow.scm");
    let vm = pass_count_vm("r7rs_case_arrow.scm").expect("vm should run r7rs_case_arrow.scm");
    println!("r7rs_case_arrow: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_port_reads() {
    let walker = pass_count_walker("r7rs_port_reads.scm");
    let vm = pass_count_vm("r7rs_port_reads.scm").expect("vm should run r7rs_port_reads.scm");
    println!("r7rs_port_reads: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_bytevector_literal() {
    let walker = pass_count_walker("r7rs_bytevector_literal.scm");
    let vm = pass_count_vm("r7rs_bytevector_literal.scm")
        .expect("vm should run r7rs_bytevector_literal.scm");
    println!("r7rs_bytevector_literal: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_char_names() {
    let walker = pass_count_walker("r7rs_char_names.scm");
    let vm = pass_count_vm("r7rs_char_names.scm").expect("vm should run r7rs_char_names.scm");
    println!("r7rs_char_names: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_string_escapes() {
    let walker = pass_count_walker("r7rs_string_escapes.scm");
    let vm =
        pass_count_vm("r7rs_string_escapes.scm").expect("vm should run r7rs_string_escapes.scm");
    println!("r7rs_string_escapes: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_pipe_identifiers() {
    let walker = pass_count_walker("r7rs_pipe_identifiers.scm");
    let vm = pass_count_vm("r7rs_pipe_identifiers.scm")
        .expect("vm should run r7rs_pipe_identifiers.scm");
    println!("r7rs_pipe_identifiers: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_port_writes() {
    let walker = pass_count_walker("r7rs_port_writes.scm");
    let vm = pass_count_vm("r7rs_port_writes.scm").expect("vm should run r7rs_port_writes.scm");
    println!("r7rs_port_writes: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_error_predicates() {
    let walker = pass_count_walker("r7rs_error_predicates.scm");
    let vm = pass_count_vm("r7rs_error_predicates.scm")
        .expect("vm should run r7rs_error_predicates.scm");
    println!("r7rs_error_predicates: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_read_error() {
    let walker = pass_count_walker("r7rs_read_error.scm");
    let vm = pass_count_vm("r7rs_read_error.scm").expect("vm should run r7rs_read_error.scm");
    println!("r7rs_read_error: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_exit() {
    let walker = pass_count_walker("r7rs_exit.scm");
    let vm = pass_count_vm("r7rs_exit.scm").expect("vm should run r7rs_exit.scm");
    println!("r7rs_exit: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_call_with_port() {
    let walker = pass_count_walker("r7rs_call_with_port.scm");
    let vm =
        pass_count_vm("r7rs_call_with_port.scm").expect("vm should run r7rs_call_with_port.scm");
    println!("r7rs_call_with_port: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_port_mgmt() {
    let walker = pass_count_walker("r7rs_port_mgmt.scm");
    let vm = pass_count_vm("r7rs_port_mgmt.scm").expect("vm should run r7rs_port_mgmt.scm");
    println!("r7rs_port_mgmt: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_eq_predicates() {
    let walker = pass_count_walker("r7rs_eq_predicates.scm");
    let vm = pass_count_vm("r7rs_eq_predicates.scm").expect("vm should run r7rs_eq_predicates.scm");
    println!("r7rs_eq_predicates: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_fill() {
    let walker = pass_count_walker("r7rs_fill.scm");
    let vm = pass_count_vm("r7rs_fill.scm").expect("vm should run r7rs_fill.scm");
    println!("r7rs_fill: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_delay_force() {
    let walker = pass_count_walker("r7rs_delay_force.scm");
    let vm = pass_count_vm("r7rs_delay_force.scm").expect("vm should run r7rs_delay_force.scm");
    println!("r7rs_delay_force: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_syntax_error() {
    let walker = pass_count_walker("r7rs_syntax_error.scm");
    let vm = pass_count_vm("r7rs_syntax_error.scm").expect("vm should run r7rs_syntax_error.scm");
    println!("r7rs_syntax_error: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_copy() {
    let walker = pass_count_walker("r7rs_copy.scm");
    let vm = pass_count_vm("r7rs_copy.scm").expect("vm should run r7rs_copy.scm");
    println!("r7rs_copy: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_to_list() {
    let walker = pass_count_walker("r7rs_to_list.scm");
    let vm = pass_count_vm("r7rs_to_list.scm").expect("vm should run r7rs_to_list.scm");
    println!("r7rs_to_list: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_bytevector_fill() {
    let walker = pass_count_walker("r7rs_bytevector_fill.scm");
    let vm =
        pass_count_vm("r7rs_bytevector_fill.scm").expect("vm should run r7rs_bytevector_fill.scm");
    println!("r7rs_bytevector_fill: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_string_ctor() {
    let walker = pass_count_walker("r7rs_string_ctor.scm");
    let vm = pass_count_vm("r7rs_string_ctor.scm").expect("vm should run r7rs_string_ctor.scm");
    println!("r7rs_string_ctor: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_string_map() {
    let walker = pass_count_walker("r7rs_string_map.scm");
    let vm = pass_count_vm("r7rs_string_map.scm").expect("vm should run r7rs_string_map.scm");
    println!("r7rs_string_map: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_ci_compare() {
    let walker = pass_count_walker("r7rs_ci_compare.scm");
    let vm = pass_count_vm("r7rs_ci_compare.scm").expect("vm should run r7rs_ci_compare.scm");
    println!("r7rs_ci_compare: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_cond_expand_lib() {
    let walker = pass_count_walker("r7rs_cond_expand_lib.scm");
    let vm =
        pass_count_vm("r7rs_cond_expand_lib.scm").expect("vm should run r7rs_cond_expand_lib.scm");
    println!("r7rs_cond_expand_lib: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_bytevector_list() {
    let walker = pass_count_walker("r7rs_bytevector_list.scm");
    let vm =
        pass_count_vm("r7rs_bytevector_list.scm").expect("vm should run r7rs_bytevector_list.scm");
    println!("r7rs_bytevector_list: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_read_default_port() {
    let walker = pass_count_walker("r7rs_read_default_port.scm");
    let vm = pass_count_vm("r7rs_read_default_port.scm")
        .expect("vm should run r7rs_read_default_port.scm");
    println!("r7rs_read_default_port: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_write_default_port() {
    let walker = pass_count_walker("r7rs_write_default_port.scm");
    let vm = pass_count_vm("r7rs_write_default_port.scm")
        .expect("vm should run r7rs_write_default_port.scm");
    println!("r7rs_write_default_port: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_binary_default_port() {
    let walker = pass_count_walker("r7rs_binary_default_port.scm");
    let vm = pass_count_vm("r7rs_binary_default_port.scm")
        .expect("vm should run r7rs_binary_default_port.scm");
    println!("r7rs_binary_default_port: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_current_error_port() {
    let walker = pass_count_walker("r7rs_current_error_port.scm");
    let vm = pass_count_vm("r7rs_current_error_port.scm")
        .expect("vm should run r7rs_current_error_port.scm");
    println!("r7rs_current_error_port: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_write_aliases() {
    let walker = pass_count_walker("r7rs_write_aliases.scm");
    let vm = pass_count_vm("r7rs_write_aliases.scm").expect("vm should run r7rs_write_aliases.scm");
    println!("r7rs_write_aliases: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}

#[test]
fn vm_conformance_r7rs_string_to_number() {
    let walker = pass_count_walker("r7rs_string_to_number.scm");
    let vm = pass_count_vm("r7rs_string_to_number.scm")
        .expect("vm should run r7rs_string_to_number.scm");
    println!("r7rs_string_to_number: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}
