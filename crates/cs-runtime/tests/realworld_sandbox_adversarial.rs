//! Adversarial real-world tests for the L1 sandbox import policy
//! (issue #15 / PR #38). These exercise scenarios that motivated the
//! security-MEDIUM fix: nested `(environment ...)` inside `eval`,
//! import-spec normalization quirks, and policy-set vs policy-cleared
//! state transitions.
//!
//! The fix added `Runtime::set_sandbox_import_policy(Option<Vec<String>>)`
//! and routed `b_environment` through a host-policy check before
//! `resolve_import_spec`. Pre-fix, a guest `(eval EXPR (environment
//! '(rnrs lists)))` could bypass the host-approved import set if
//! `(rnrs lists)` was simply absent from the catalog (the pre-fix test
//! passed for the wrong reason). Post-fix, the host policy is the
//! causal barrier.

use cs_diag::Diagnostic;
use cs_runtime::Runtime;

fn evaluate(rt: &mut Runtime, src: &str) -> Result<String, Diagnostic> {
    rt.eval_str("<adv>", src)
        .map(|v| rt.format_value(&v, cs_core::WriteMode::Display))
}

// ---------------- Direct policy enforcement ----------------

#[test]
fn disallowed_library_is_rejected_when_policy_is_strict() {
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into()]));
    let err = evaluate(&mut rt, "(environment '(rnrs lists))").unwrap_err();
    assert!(
        err.message.contains("rnrs lists"),
        "error must name the rejected library; got: {}",
        err.message
    );
}

#[test]
fn allowed_library_is_accepted_when_policy_lists_it() {
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into(), "(rnrs lists)".into()]));
    let r = evaluate(
        &mut rt,
        "(environment? (environment '(rnrs base) '(rnrs lists)))",
    );
    assert!(
        r.is_ok(),
        "approved libraries must not be blocked; got: {:?}",
        r
    );
}

#[test]
fn empty_policy_means_nothing_is_allowed() {
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec![]));
    let err = evaluate(&mut rt, "(environment '(rnrs base))").unwrap_err();
    assert!(
        err.message.contains("rnrs base"),
        "empty policy must reject even rnrs base; got: {}",
        err.message
    );
}

#[test]
fn no_policy_means_unrestricted() {
    let mut rt = Runtime::new();
    // No set_sandbox_import_policy call — runtime defaults to None.
    let r = evaluate(&mut rt, "(environment? (environment '(rnrs base)))");
    assert!(r.is_ok(), "no policy means anything in the catalog works");
}

// ---------------- Nested-eval bypass — the PR #38 motivation ----

#[test]
fn nested_eval_cannot_widen_the_import_set() {
    // The exact pre-fix exploit. A guest with policy `(rnrs base)` only
    // attempts to widen by calling `(eval EXPR (environment '(rnrs
    // lists)))`. Post-fix, the inner `(environment ...)` triggers the
    // host-policy check and is rejected.
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into()]));
    let err = evaluate(&mut rt, "(eval '(list 1 2 3) (environment '(rnrs lists)))").unwrap_err();
    assert!(
        err.message.contains("rnrs lists"),
        "nested (eval (environment ...)) must be blocked; got: {}",
        err.message
    );
}

// (deleted: doubly_nested_eval_cannot_widen)
//
// The previous version of this test was flawed in premise: the outer
// `(environment '(rnrs base))` doesn't bind `eval` or `environment`,
// so the inner form failed with an unbound-variable error before the
// policy check could reject the disallowed `(rnrs lists)`. The
// single-level `nested_eval_cannot_widen_the_import_set` test above
// already covers the exploit path PR #38 was written for. A real
// multi-level test would require constructing a base environment
// that includes the eval procedure but not (rnrs lists) — left as
// follow-up work alongside the cs-runtime test for namespaced eval
// (the future M11 cs-agent sandbox tests).

// ---------------- Policy mutation mid-session ----------------

#[test]
fn policy_can_be_relaxed_at_runtime() {
    // The host can widen the policy. Starts strict — call fails;
    // policy widened — call succeeds.
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into()]));
    assert!(evaluate(&mut rt, "(environment '(rnrs lists))").is_err());

    rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into(), "(rnrs lists)".into()]));
    assert!(evaluate(&mut rt, "(environment? (environment '(rnrs lists)))").is_ok());
}

#[test]
fn policy_can_be_tightened_at_runtime() {
    // Symmetric: starts unrestricted, then tightened.
    let mut rt = Runtime::new();
    assert!(evaluate(&mut rt, "(environment? (environment '(rnrs lists)))").is_ok());

    rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into()]));
    assert!(evaluate(&mut rt, "(environment '(rnrs lists))").is_err());
}

#[test]
fn clearing_policy_with_none_restores_unrestricted_access() {
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into()]));
    assert!(evaluate(&mut rt, "(environment '(rnrs lists))").is_err());

    rt.set_sandbox_import_policy(None);
    assert!(evaluate(&mut rt, "(environment? (environment '(rnrs lists)))").is_ok());
}

// ---------------- Import-spec normalization ----------------

#[test]
fn spec_normalization_handles_extra_whitespace() {
    // The host writes `"(rnrs   base)"` (extra whitespace); the
    // policy check normalizes both sides to space-joined canonical
    // form before comparing.
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec!["(rnrs   base)".into()]));
    let r = evaluate(&mut rt, "(environment? (environment '(rnrs base)))");
    assert!(r.is_ok(), "normalization must collapse whitespace");
}

#[test]
fn allowed_check_is_per_spec_not_per_name() {
    // The policy lists `(rnrs lists)` but not `(rnrs base)`. A
    // `(rnrs base)` import must STILL be rejected — the check is
    // per-import-spec, not by name substring.
    let mut rt = Runtime::new();
    rt.set_sandbox_import_policy(Some(vec!["(rnrs lists)".into()]));
    let err = evaluate(&mut rt, "(environment '(rnrs base))").unwrap_err();
    assert!(
        err.message.contains("rnrs base"),
        "per-spec check rejects unrelated libraries; got: {}",
        err.message
    );
}
