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

// bitwise_eval.scm requires `eval`, which is not yet ported to the VM tier
// (would need to invoke the parser+expander+VM compile mid-execution); the
// walker handles it via re-entry. Skipped on VM until that bridge is added.

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

// let_values_dynwind.scm uses with-output-to-string (current_output_port).
// dynamic-wind itself works on VM, but the file's tests rely on output-port
// state that isn't yet bridged. Skipped pending current-output-port port.

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

// bytevectors_misc.scm uses with-output-to-string (needs current_output_port
// state, not yet ported to VM). Skipped on VM until port-state bridge lands.

#[test]
fn vm_conformance_hashtables() {
    let walker = pass_count_walker("hashtables.scm");
    let vm = pass_count_vm("hashtables.scm").expect("vm should run hashtables.scm");
    println!("hashtables: walker={} vm={}", walker, vm);
    assert_eq!(walker, vm);
}
