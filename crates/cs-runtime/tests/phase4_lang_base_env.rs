//! R6RS++ Phase 4 — `#!lang` `base-env` export (issue #70).
//!
//! When `(lang NAME)` declares `(export base-env ...)` and binds
//! the name to an environment value (built via `environment` or
//! `make-namespace`), the file body evaluates against that env
//! instead of the runtime's global top env. Integrates with ADR
//! 0015 L1 sandboxing — the lang library effectively gates which
//! bindings the body sees.
//!
//! Coverage:
//! - base-env constructed via `(environment '(rnrs base))` — body
//!   sees only `(rnrs base)` bindings; names imported elsewhere
//!   are out of scope
//! - base-env constructed via `(make-namespace ...)` — mutable
//!   namespace lets the body `set!` and `define`
//! - base-env not exported → behaves as before (full top env)
//! - base-env exported but not a valid environment → typed error
//! - `reader` + `base-env` together — reader-produced datums run
//!   against the restricted env

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn install(rt: &mut Runtime, src: &str) {
    rt.eval_str("<lang-install>", src).unwrap();
}

// ---- restricted base-env via `environment` ----

#[test]
fn base_env_restricts_body_visibility() {
    let mut rt = Runtime::new();
    // The body sees only `(rnrs base)` — `(rnrs lists)` bindings
    // (like `fold-left`) won't resolve. Confirms the env actually
    // narrows visibility.
    install(
        &mut rt,
        "(library (lang only-base)
           (export base-env)
           (import (rnrs))
           (define base-env (environment '(rnrs base))))",
    );
    let v = rt
        .eval_str(
            "<t>",
            "#!lang only-base\n\
             (+ 1 2 3)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

#[test]
fn base_env_blocks_bindings_outside_the_env() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang only-base-2)
           (export base-env)
           (import (rnrs))
           (define base-env (environment '(rnrs base))))",
    );
    // `fold-left` lives in `(rnrs lists)`, not `(rnrs base)`. The
    // body's reference should fail to resolve against the
    // restricted env.
    let err = rt
        .eval_str(
            "<t>",
            "#!lang only-base-2\n\
             (fold-left + 0 '(1 2 3))",
        )
        .expect_err("fold-left should be out of scope");
    let s = format!("{}", err);
    assert!(
        s.contains("fold-left") || s.contains("undefined"),
        "diagnostic should mention the missing binding: {}",
        s
    );
}

// ---- mutable namespace via `make-namespace` ----

#[test]
fn make_namespace_base_env_allows_define_in_body() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang mutable-ns)
           (export base-env)
           (import (rnrs))
           (define base-env (make-namespace '(rnrs base))))",
    );
    let v = rt
        .eval_str(
            "<t>",
            "#!lang mutable-ns\n\
             (define x 100)\n\
             (define y 23)\n\
             (+ x y)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "123");
}

// ---- no base-env exported: default behaviour ----

#[test]
fn lang_without_base_env_uses_full_top_env() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang marker)
           (export marker)
           (import (rnrs))
           (define marker 'present))",
    );
    // No base-env → body has the full runtime top env, including
    // `fold-left` (which lives in `(rnrs lists)`).
    let v = rt
        .eval_str(
            "<t>",
            "#!lang marker\n\
             (fold-left + 0 '(1 2 3))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

// ---- error path: base-env not an environment ----

#[test]
fn base_env_must_be_an_environment_value() {
    let mut rt = Runtime::new();
    install(
        &mut rt,
        "(library (lang bad-env)
           (export base-env)
           (import (rnrs))
           (define base-env 42))",
    );
    let err = rt
        .eval_str("<t>", "#!lang bad-env\n")
        .expect_err("non-env base-env should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("(lang bad-env)") && s.contains("environment"),
        "diagnostic should name the lang and 'environment': {}",
        s
    );
}

// ---- composition: reader + base-env ----

#[test]
fn reader_output_runs_against_base_env() {
    let mut rt = Runtime::new();
    // Lang exports BOTH reader (which produces a fixed body) and
    // base-env (restricted to (rnrs base)). The reader's output
    // must evaluate against the restricted env, not the full top
    // env. We pin this by having the reader emit a form that
    // would work fine against rnrs base.
    install(
        &mut rt,
        "(library (lang reader-plus-env)
           (export reader base-env)
           (import (rnrs))
           (define base-env (environment '(rnrs base)))
           (define (reader body-str) '((+ 10 20 30))))",
    );
    let v = rt.eval_str("<t>", "#!lang reader-plus-env\n").unwrap();
    assert_eq!(disp(&rt, &v), "60");
}

#[test]
fn reader_output_blocked_by_restrictive_base_env() {
    let mut rt = Runtime::new();
    // Same composition: reader emits a form that uses fold-left
    // (which is NOT in (rnrs base)). Even though the reader
    // itself runs against the full env (it has access to its
    // imports), the body it returns runs against base-env.
    install(
        &mut rt,
        "(library (lang strict)
           (export reader base-env)
           (import (rnrs))
           (define base-env (environment '(rnrs base)))
           (define (reader body-str) '((fold-left + 0 '(1 2 3)))))",
    );
    let err = rt
        .eval_str("<t>", "#!lang strict\n")
        .expect_err("fold-left should be blocked by restricted env");
    let s = format!("{}", err);
    assert!(
        s.contains("fold-left") || s.contains("undefined"),
        "diagnostic should mention the missing binding: {}",
        s
    );
}
