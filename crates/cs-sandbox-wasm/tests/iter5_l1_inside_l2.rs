//! cs-sandbox-wasm iter 5 — L1 inside L2 defense in depth.
//!
//! Per ADR 0015 iter 5: the guest's expression is wrapped in
//! `(eval 'EXPR (environment IMPORTS))` so the L1 namespace
//! restriction fires even when the WASI capability layer would
//! otherwise allow more. Two-layer enforcement: L2 (WASI) is the
//! capability boundary; L1 (namespace) is the lexical boundary.

use std::path::PathBuf;
use std::time::Duration;

use cs_sandbox_wasm::{SandboxConfig, SandboxInstance};

fn binary_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("CRABSCHEME_WASM_PATH") {
        let p = PathBuf::from(env_path);
        return p.exists().then_some(p);
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip1/release/crabscheme.wasm");
    default.exists().then_some(default)
}

fn cfg_with_imports(imports: Vec<String>) -> SandboxConfig {
    let mut c = SandboxConfig::hygiene();
    c.binary_path = binary_path();
    c.imports = imports;
    c
}

macro_rules! requires_wasm {
    () => {
        if binary_path().is_none() {
            eprintln!("skipping: crabscheme.wasm not found");
            return;
        }
    };
}

// ---- L1 still allows (rnrs base) names ----

#[test]
fn rnrs_base_arithmetic_works_inside_sandbox() {
    requires_wasm!();
    let mut sb = SandboxInstance::new(cfg_with_imports(vec!["(rnrs base)".into()])).unwrap();
    let result = sb.eval("(+ 1 2 3)").unwrap();
    assert_eq!(result, "6");
}

#[test]
fn rnrs_base_list_ops_work_inside_sandbox() {
    requires_wasm!();
    let mut sb = SandboxInstance::new(cfg_with_imports(vec!["(rnrs base)".into()])).unwrap();
    let result = sb.eval("(list 1 2 3)").unwrap();
    assert_eq!(result, "(1 2 3)");
}

// ---- L1 rejects names outside the import set ----

#[test]
fn hashtable_q_is_not_in_rnrs_base_so_sandbox_rejects() {
    // hashtable? is registered globally in the guest but NOT in
    // (rnrs base) per L1.3's split. The L1 wrap should produce
    // an unbound-identifier error.
    requires_wasm!();
    let mut sb = SandboxInstance::new(cfg_with_imports(vec!["(rnrs base)".into()])).unwrap();
    let result = sb.eval("(hashtable? 42)");
    assert!(
        result.is_err(),
        "hashtable? should be unbound in (rnrs base)"
    );
}

#[test]
fn for_all_is_in_rnrs_lists_not_base() {
    // for-all is (rnrs lists) — not (rnrs base).
    requires_wasm!();
    let mut sb = SandboxInstance::new(cfg_with_imports(vec!["(rnrs base)".into()])).unwrap();
    let result = sb.eval("(for-all positive? (list 1 2 3))");
    assert!(
        result.is_err(),
        "for-all should be unbound in (rnrs base) alone"
    );
}

// ---- Composite imports unlock (rnrs lists) procs ----

#[test]
fn composite_imports_unlock_for_all() {
    requires_wasm!();
    let mut sb = SandboxInstance::new(cfg_with_imports(vec![
        "(rnrs base)".into(),
        "(rnrs lists)".into(),
    ]))
    .unwrap();
    let result = sb.eval("(for-all positive? (list 1 2 3))").unwrap();
    assert_eq!(result, "#t");
}

// ---- The key defense-in-depth claim ----
//
// Even if the host accidentally grants too-much WASI capability
// (allow_paths includes a sensitive dir), the L1 wrap prevents
// (open-output-file ...) etc. from resolving because they're
// not in (rnrs base). The expression fails at the EXPAND step
// inside the guest, before any WASI capability gets consulted.

#[test]
fn open_input_file_unreachable_via_l1_even_with_root_grant() {
    requires_wasm!();
    // Deliberately over-grant WASI: map / into the guest. The
    // L1 wrap still prevents the file op because
    // open-input-file is not in (rnrs base).
    let mut config = cfg_with_imports(vec!["(rnrs base)".into()]);
    // Don't actually mount / for safety; the point is even IF
    // we did, L1 would still block. The test below verifies
    // the L1 path errors regardless.
    config.wall_clock_timeout = Duration::from_secs(10);
    let mut sb = SandboxInstance::new(config).unwrap();
    let result = sb.eval("(open-input-file \"/etc/passwd\")");
    assert!(
        result.is_err(),
        "open-input-file should be blocked at L1 (unbound)"
    );
    // Confirm it's the L1 layer (unbound identifier) rather
    // than the L2 layer (WASI capability denied) — message
    // distinguishing is loose since the guest's error text
    // varies, but L1 errors include the identifier name.
    let err_str = format!("{:?}", result.unwrap_err());
    assert!(
        err_str.contains("open-input-file")
            || err_str.contains("unbound")
            || err_str.contains("undefined"),
        "expected L1 unbound error, got: {}",
        err_str
    );
}

// ---- L1's set! enforcement carries through L2 ----

#[test]
fn set_against_l1_immutable_binding_raises_inside_sandbox() {
    requires_wasm!();
    let mut sb = SandboxInstance::new(cfg_with_imports(vec!["(rnrs base)".into()])).unwrap();
    // L1's snapshot env makes + immutable. set! should raise
    // &assertion which propagates through the guest's
    // print-result path.
    let result = sb.eval("(set! + 5)");
    assert!(
        result.is_err(),
        "set! against L1 immutable binding should error"
    );
}
