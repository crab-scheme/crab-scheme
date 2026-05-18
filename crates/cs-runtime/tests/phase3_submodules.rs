//! R6RS++ Phase 3B — submodules.
//!
//! `(submodule NAME body...)` inside a library is lifted to a
//! sibling library `(library (parent... NAME) (export) (import)
//! body...)` and registered alongside its parent. The submodule's
//! body sees the parent's defines because the library system is
//! still flat / global at this milestone.
//!
//! Optional leading `(export ...)` / `(import ...)` clauses inside
//! a submodule are honored if present.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- basic submodule lifting ----

#[test]
fn submodule_runs_after_parent_body() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(library (math basic)
               (export square)
               (import (rnrs))
               (define (square n) (* n n))
               (submodule tests
                 (define test-result (square 5))))
             test-result",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "25");
}

#[test]
fn submodule_sees_parent_define() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(library (web server)
               (export start)
               (import (rnrs))
               (define (start port) (* port 2))
               (submodule tests
                 (define test-port (start 8080))))
             test-port",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "16160");
}

#[test]
fn multiple_submodules_run_in_order() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(library (counter)
               (export reset bump)
               (import (rnrs))
               (define n 0)
               (define (reset) (set! n 0))
               (define (bump) (set! n (+ n 1)))
               (submodule one
                 (bump) (bump))
               (submodule two
                 (bump) (bump) (bump)))
             n",
        )
        .unwrap();
    // one bumps twice; two bumps three more; total 5.
    assert_eq!(disp(&rt, &v), "5");
}

// ---- registered as sibling library ----

#[test]
fn submodule_library_registered_under_extended_name() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        "(library (web server)
           (export start)
           (import (rnrs))
           (define (start port) port)
           (submodule tests
             (define dummy 1)))",
    )
    .unwrap();
    // Importing the lifted submodule's library should succeed
    // (a re-import is a no-op; this just verifies registration).
    let v = rt
        .eval_str("<t>", "(import (web server tests)) 'ok")
        .unwrap();
    assert_eq!(disp(&rt, &v), "ok");
}

// ---- empty submodule ----

#[test]
fn empty_submodule_is_valid() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(library (only-parent)
               (export x)
               (import (rnrs))
               (define x 1)
               (submodule empty))
             x",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "1");
}

// ---- error cases ----

#[test]
fn submodule_without_name_errors() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(library (broken)
               (export)
               (import (rnrs))
               (submodule))",
        )
        .expect_err("missing submodule name");
    let s = format!("{}", err);
    assert!(s.contains("submodule needs a name"), "got: {}", s);
}

#[test]
fn submodule_name_must_be_symbol() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(library (broken)
               (export)
               (import (rnrs))
               (submodule (not-a-symbol) (define x 1)))",
        )
        .expect_err("name must be symbol");
    let s = format!("{}", err);
    assert!(
        s.contains("submodule name must be a single identifier"),
        "got: {}",
        s
    );
}

// ---- optional clauses ----

#[test]
fn submodule_with_explicit_export_clause() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(library (top)
               (export)
               (import (rnrs))
               (submodule sub
                 (export sub-val)
                 (define sub-val 99)))
             (import (top sub))
             sub-val",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "99");
}

#[test]
fn submodule_can_define_and_reference_parent_state() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(library (state)
               (export get inc)
               (import (rnrs))
               (define counter 0)
               (define (get) counter)
               (define (inc) (set! counter (+ counter 1)))
               (submodule warm-up
                 (inc) (inc) (inc)))
             (get)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "3");
}
