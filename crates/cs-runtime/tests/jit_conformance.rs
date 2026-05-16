//! M6 iter 10 — broader JIT differential coverage via the
//! conformance suite.
//!
//! For each chosen conformance file, run on three configurations:
//! 1. Walker (`Runtime::eval_str`)
//! 2. VM no JIT (`Runtime::eval_str_via_vm`)
//! 3. VM + JIT (`Runtime::eval_str_via_vm` after `install_jit()`)
//!
//! Assert all three produce the same `(__test-summary__)` pass
//! count. The JIT translator silently falls back on closures it
//! can't lower, so the JIT-enabled run is a strict subset (in
//! observable behavior) of the no-JIT VM run — any divergence is a
//! bug.

use std::fs;

use cs_core::{Value, WriteMode};
use cs_runtime::Runtime;

fn workspace_root() -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    format!("{}/../..", manifest)
}

fn read_test(file: &str) -> (String, String) {
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
    (prelude, body)
}

fn extract_pass_count(rt: &Runtime, v: &Value) -> u64 {
    let s = rt.format_value(v, WriteMode::Write);
    let trimmed = s.trim_start_matches('(');
    let first = trimmed.split_whitespace().next().unwrap_or("0");
    first.parse().unwrap_or(0)
}

fn pass_count_walker(file: &str) -> u64 {
    let (prelude, body) = read_test(file);
    let mut rt = Runtime::new();
    rt.eval_str("_prelude.scm", &prelude).unwrap();
    rt.eval_str(file, &body).unwrap();
    let summary = rt.eval_str("<harness>", "(__test-summary__)").unwrap();
    extract_pass_count(&rt, &summary)
}

fn pass_count_vm_no_jit(file: &str) -> u64 {
    let (prelude, body) = read_test(file);
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("_prelude.scm", &prelude).unwrap();
    rt.eval_str_via_vm(file, &body).unwrap();
    let summary = rt
        .eval_str_via_vm("<harness>", "(__test-summary__)")
        .unwrap();
    extract_pass_count(&rt, &summary)
}

fn pass_count_vm_with_jit(file: &str) -> u64 {
    let (prelude, body) = read_test(file);
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("_prelude.scm", &prelude).unwrap();
    rt.eval_str_via_vm(file, &body).unwrap();
    let summary = rt
        .eval_str_via_vm("<harness>", "(__test-summary__)")
        .unwrap();
    extract_pass_count(&rt, &summary)
}

fn assert_three_tier_pass_count(file: &str) {
    let walker = pass_count_walker(file);
    let vm = pass_count_vm_no_jit(file);
    let jit = pass_count_vm_with_jit(file);
    println!("{file}: walker={walker} vm={vm} jit={jit}");
    assert_eq!(walker, vm, "{file}: walker vs vm-no-jit");
    assert_eq!(vm, jit, "{file}: vm-no-jit vs vm-jit");
    // Sanity: the file actually has tests.
    assert!(walker > 0, "{file}: 0 passes — empty test file?");
}

// One #[test] per file so a regression on one doesn't mask the
// others. Files chosen to span the JIT-translatable subset
// (arithmetic, booleans, equality, control flow) and the
// rejected-fallthrough subset (strings, lists, ports — which the
// JIT silently declines and the VM handles).

#[test]
fn jit_conformance_arithmetic() {
    assert_three_tier_pass_count("arithmetic.scm");
}

#[test]
fn jit_conformance_booleans() {
    assert_three_tier_pass_count("booleans.scm");
}

#[test]
fn jit_conformance_equality() {
    assert_three_tier_pass_count("equality.scm");
}

#[test]
fn jit_conformance_case_and_assoc() {
    assert_three_tier_pass_count("case_and_assoc.scm");
}

#[test]
fn jit_conformance_char_extras() {
    assert_three_tier_pass_count("char_extras.scm");
}

#[test]
fn jit_conformance_call_cc() {
    // Continuations — the JIT silently declines anything that ends
    // up using call/cc semantics; pass count must still match.
    assert_three_tier_pass_count("call_cc.scm");
}

#[test]
fn jit_conformance_cross_lambda_loop() {
    // Regression test for the cross-lambda Fixnum-return loop bug.
    // Pre-iter3 this produced garbage on --tier vm-jit. Iter3
    // (53207f2) inadvertently fixed it by adding BoxTyped support
    // in the uniform-NB tier so the loop body no longer falls back
    // to specialized with broken return-type inference. See
    // docs/research/jit_loop_cross_lambda_bug.md.
    assert_three_tier_pass_count("jit_cross_lambda_loop.scm");
}
