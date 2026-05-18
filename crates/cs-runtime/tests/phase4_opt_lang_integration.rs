//! Phase-4-opt iter 5 — `#!lang` ↔ optimizer-pass integration.
//!
//! Demonstrates that a `(lang foo)` library whose top level
//! calls `(install-optimizer-pass! ...)` activates the named
//! pass for subsequent compilation in the file declaring
//! `#!lang foo`. This is the spec example for "domain-specific
//! optimizer pass via #!lang."
//!
//! The lib/lang/opt-fold.scm library is the minimum demonstration:
//! it installs `'constant-fold`. A production lang library might
//! install several richer passes, or even register its own Rust-
//! defined plugin pass (via the embedder's startup hook).

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn fresh_rt() -> Runtime {
    cs_opt::clear_active_passes();
    Runtime::new()
}

fn load_opt_fold(rt: &mut Runtime) {
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/lang/opt-fold.scm");
    let src = std::fs::read_to_string(&path).unwrap();
    rt.eval_str("<opt-fold>", &src).unwrap();
}

// ---- baseline: no #!lang means no install ----

#[test]
fn baseline_no_lang_header_means_no_passes_installed() {
    let mut rt = fresh_rt();
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

// ---- #!lang opt-fold ----

#[test]
fn lang_opt_fold_installs_constant_fold() {
    let mut rt = fresh_rt();
    // Load the lang library — usually done by the #!lang header
    // rewriter (Phase 3C), simulated directly here so the test
    // doesn't depend on library-resolution paths working in
    // every test environment.
    load_opt_fold(&mut rt);
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
}

#[test]
fn hashlang_header_loads_lang_library_and_installs() {
    let mut rt = fresh_rt();
    // First make `(lang opt-fold)` resolvable by loading its
    // library declaration. The `#!lang` header then expands to
    // `(import (lang opt-fold))`, which triggers the library's
    // top-level install. The Phase 3C MVP doesn't auto-locate
    // lang libs from disk — that's tracked as the iter
    // "register a lib-resolver." Until then this two-step is
    // the supported pattern.
    load_opt_fold(&mut rt);
    let v = rt
        .eval_str(
            "<t>",
            "#!lang opt-fold\n\
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
}

// ---- subsequent eval sees the pass installed ----

#[test]
fn code_after_lang_load_sees_installed_pass() {
    let mut rt = fresh_rt();
    load_opt_fold(&mut rt);
    // First eval after install sees the pass; (+ 1 2) still
    // computes correctly because constant-fold is a sound
    // optimization (and trivial code paths are unaffected).
    let v = rt.eval_str("<t>", "(+ 1 2)").unwrap();
    assert_eq!(disp(&rt, &v), "3");
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
}

// ---- explicit cleanup mirrors typical 'use'/'unuse' pattern ----

#[test]
fn user_can_explicitly_remove_after_use() {
    let mut rt = fresh_rt();
    load_opt_fold(&mut rt);
    let v = rt
        .eval_str(
            "<t>",
            "(remove-optimizer-pass! 'constant-fold)
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

// ---- composition: multiple lang libraries stack ----

#[test]
fn multiple_lang_loads_accumulate_passes() {
    // Two demo lang libs both install different passes; both
    // end up in the active list (idempotent install means
    // re-importing the first wouldn't double-add).
    let mut rt = fresh_rt();
    load_opt_fold(&mut rt);
    // Second "lang" inline — a real production library would
    // live at lib/lang/whatever.scm.
    rt.eval_str(
        "<inline-lang>",
        "(library (lang opt-dead)
           (export)
           (import (rnrs))
           (install-optimizer-pass! 'dead-block-elim))",
    )
    .unwrap();
    rt.eval_str("<t>", "(import (lang opt-dead))").unwrap();
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold dead-block-elim)");
}
