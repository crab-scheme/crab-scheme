//! Pin-test: `load-module!` accepts only data-shaped exports today.
//!
//! `cs-runtime::builtins::beam::to_sendable_in` rejects `Value::Procedure`,
//! which means modules cannot carry code (closures) — only the
//! fixnum / string / symbol / pair / vector / bytevector value shapes
//! that `SendableValue` enumerates.
//!
//! This is the prerequisite gap for #29 (`B7: JIT-invalidation on hot
//! reload`): the "stale JIT body" scenario in the issue body cannot
//! occur until a procedure can reach the version registry in the first
//! place. ADR 0034 documents the two paths past this wall (Send heaps
//! vs. per-actor rehydration) and the deferral rationale.
//!
//! When that wall moves, the third test below (`load_module_eventually_
//! accepts_procedures`) is the regression to flip from `should_panic` to
//! a positive assertion: a tier-uppable lambda is loaded into a module,
//! tier-ups, gets reloaded with a new version, and the new version's
//! body runs (verifying the JIT-invalidation path is correct end-to-end).

#![cfg(feature = "actor")]

use cs_runtime::Runtime;

mod common;

/// Sanity baseline: data-shaped exports work end-to-end through the
/// `to_sendable_in` → `from_sendable` round-trip. This is the
/// contract `beam_counter_migration` exercises in a fuller form;
/// asserted here as the floor so the next two tests' bracketing
/// makes sense.
#[test]
fn data_shaped_module_exports_round_trip_through_to_sendable() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (load-module! 'issue29-data
          '(("init-state"     . 0)
            ("schema-version" . 1)))
    "#,
    )
    .expect("data-only load-module!");

    let init = rt
        .eval_str("<t>", r#"(lookup-code 'issue29-data "init-state")"#)
        .expect("lookup init-state");
    // The exact runtime decode shape isn't important for this
    // test — only that the call succeeded and the value resolved
    // to *something*. (We assert specifically below that the
    // procedure variant doesn't even get this far.)
    assert!(
        format!("{:?}", init).contains('0'),
        "init-state should resolve to 0; got {:?}",
        init
    );
}

/// The actual `to_sendable_in` rejection — the current 1.0 contract.
/// `load-module!` errors at the boundary when an export value is a
/// procedure. ADR 0034 documents why this is acceptable for 1.0 (no
/// path exists that would benefit from JIT invalidation since no
/// procedure can be in the registry).
#[test]
fn load_module_rejects_procedure_exports_today() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            r#"
            (load-module! 'issue29-proc
              (list (cons "fn" (lambda () 42))))
        "#,
        )
        .expect_err("load-module! should reject procedure exports today");

    // The error message must point at both surfaces (cross-actor send
    // AND load-module!) so a contributor running into this for the
    // first time sees the prerequisite (ADR 0034) without having to
    // grep for context.
    let msg = format!("{}", err);
    assert!(
        msg.contains("procedures cannot cross actor boundaries"),
        "error should retain the cross-actor wording: {}",
        msg
    );
    assert!(
        msg.contains("load-module!") && msg.contains("ADR 0034"),
        "error should mention the load-module! surface + ADR 0034: {}",
        msg
    );
}

/// Forward-looking placeholder. When #29's prerequisite lands —
/// procedures can be `to_sendable_in`'d, via either Send heaps or
/// per-actor rehydration — this test flips from `#[ignore]` to a
/// positive assertion of the end-to-end JIT-invalidation contract:
/// after `(load-module! 'm v2-exports)`, calls to the v2 procedure
/// must run v2's body, not a stale JIT-compiled v1 body.
///
/// Kept as a literal `#[ignore]` (not gated behind a feature flag)
/// so the test surface stays exactly one location to revisit. ADR
/// 0034 captures the exact next step in code terms.
#[test]
#[ignore = "blocked on #29's prerequisite — procedures-as-exports \
            (Send heaps or per-actor rehydration). See ADR 0034."]
fn load_module_eventually_accepts_procedures_and_invalidates_jit() {
    // Skeleton — kept as documentation of the eventual shape, not
    // as a runnable test. When the wall moves, the assertions below
    // become the acceptance criterion for #29.
    let mut rt = Runtime::new();

    // 1. Load v1 of a tier-up-able procedure.
    rt.eval_str(
        "<t>",
        r#"
        (load-module! 'issue29-future
          (list (cons "fn" (lambda () 1))))
    "#,
    )
    .expect("v1 load");

    // 2. Stash the v1 procedure value + call it enough to JIT.
    //    (Current implementation: this whole eval_str errors at the
    //    `to_sendable_in` boundary before reaching the tier-up loop.)
    let _v1 = rt
        .eval_str("<t>", r#"(lookup-code 'issue29-future "fn")"#)
        .expect("v1 lookup");

    // 3. Reload with v2 returning a different value.
    rt.eval_str(
        "<t>",
        r#"
        (load-module! 'issue29-future
          (list (cons "fn" (lambda () 2))))
    "#,
    )
    .expect("v2 load");

    // 4. The v2 lookup must return v2's body (not the JIT-cached v1).
    let r = rt
        .eval_str("<t>", r#"((lookup-code 'issue29-future "fn"))"#)
        .expect("v2 call");
    assert_eq!(
        format!("{:?}", r).contains('2'),
        true,
        "v2 call must return 2 (not v1's 1) — JIT-invalidation contract: {:?}",
        r
    );
}
