//! Macro-expansion verification for `lib/beam/prelude.scm`.
//!
//! The exit-doc honesty list called out: "the Scheme macros in
//! lib/beam/prelude.scm have not been macro-expanded against
//! the real expander." This test attempts the simplest
//! macro-expansion paths.
//!
//! What we test: try to evaluate a self-contained subset of the
//! prelude — one macro form at a time — against the actual
//! Runtime + expander. Each test asserts ONE claim, so a
//! failure points at exactly which surface area needs the
//! expander work.

#![cfg(feature = "actor")]

use cs_runtime::Runtime;

/// `(call ...)` is a `case-lambda` over the (send + receive)
/// builtins. Verifies (a) `case-lambda` is supported, (b) the
/// definition installs into the top-level env.
#[test]
fn case_lambda_call_definition_loads() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define call
          (case-lambda
            ((pid msg) (call pid msg #f))
            ((pid msg timeout-ms)
             ;; Test the macro expansion only — don't actually send.
             (if timeout-ms
                 (list 'timed pid msg timeout-ms)
                 (list 'untimed pid msg)))))
    "#,
    )
    .expect("case-lambda call should define cleanly");

    // Invoke each arity.
    let v1 = rt.eval_str("<t>", "(call 'p1 'msg1)").expect("2-arg call");
    assert_eq!(
        rt.format_value(&v1, cs_core::WriteMode::Display),
        "(untimed p1 msg1)"
    );

    let v2 = rt
        .eval_str("<t>", "(call 'p2 'msg2 100)")
        .expect("3-arg call");
    assert_eq!(
        rt.format_value(&v2, cs_core::WriteMode::Display),
        "(timed p2 msg2 100)"
    );
}

/// Verifies the simpler shape of the `(receive (pat action))`
/// macro — single clause, no ellipsis, no `after`. The
/// multi-clause + ellipsis + literal-keyword form from the
/// prelude hits an expander edge (empty-application '()' on
/// the cond-with-ellipsis expansion); tracked as ignored test
/// below.
#[test]
fn receive_macro_single_clause_compiles() {
    let mut rt = Runtime::new();
    let result = rt
        .eval_str(
            "<t>",
            r#"
        (define raw-receive (lambda args 'stubbed-msg))
        (define-syntax match-and-bind
          (syntax-rules ()
            ((_ msg pat action)
             (if (equal? msg 'pat) action #f))))
        (define-syntax receive-1
          (syntax-rules ()
            ((_ (pat action))
             (let ((msg (raw-receive #f)))
               (or (match-and-bind msg pat action)
                   'no-match)))))
        (receive-1 (stubbed-msg 42))
    "#,
        )
        .expect("simpler receive-1 macro compiles + dispatches");

    assert_eq!(rt.format_value(&result, cs_core::WriteMode::Display), "42");
}

/// The full prelude-shape receive macro with both ellipsis
/// clauses AND a trailing literal-keyword `after` clause.
/// Originally failed with "empty application '()'" before the
/// cs-diag span-merge fix; verifies expansion + dispatch end
/// to end through the with-after form.
#[test]
fn receive_macro_with_after_clause_compiles_and_dispatches() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            r#"
        (define raw-receive (lambda args 'm))
        (define-syntax match-and-bind
          (syntax-rules ()
            ((_ m p a) (if (equal? m 'p) a #f))))
        (define-syntax receive
          (syntax-rules (after)
            ((_ (pat action) ...)
             (let loop ()
               (let ((msg (raw-receive #f)))
                 (cond
                   ((match-and-bind msg pat action) ...)
                   (else (loop))))))
            ((_ (pat action) ... (after ms ta))
             (let ((msg (raw-receive ms)))
               (cond ((match-and-bind msg pat action) ...)
                     (else ta))))))
        (receive (m 1) (after 10 0))
    "#,
        )
        .expect("receive with after clause expands + dispatches");
    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "1");
}

/// Cross-eval_str macro invocation: define the macro in unit
/// A, invoke it from unit B. Pre-fix this hit a debug-assert
/// in `cs_diag::Span::merge` ("cannot merge spans from
/// different files") because `cs_expand::rebuild_list` was
/// merging the template's span (file A) with substituted user
/// arg spans (file B). The fix in cs-diag tolerates cross-file
/// merges by preferring `self`'s span (the location being
/// extended). This test pins the fix in place.
#[test]
fn receive_macro_cross_unit_invocation() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<unit-a>",
        r#"
        (define raw-receive (lambda args 'msg))
        (define-syntax match-and-bind
          (syntax-rules ()
            ((_ m p a) (if (equal? m 'p) a #f))))
        (define-syntax receive
          (syntax-rules (after)
            ((_ (pat action) ... (after ms ta))
             (let ((msg (raw-receive ms)))
               (cond ((match-and-bind msg pat action) ...)
                     (else ta))))))
    "#,
    )
    .expect("unit-a macro defs load");

    let v = rt
        .eval_str("<unit-b>", "(receive (msg 1) (after 10 0))")
        .expect("unit-b invocation expands cleanly");
    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "1");
}

/// define-record-type for `<child-spec>` from the prelude.
/// Verifies records work — supervisor needs this.
#[test]
fn child_spec_record_type_loads() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-record-type <child-spec>
          (make-child-spec id start-thunk restart shutdown child-type)
          child-spec?
          (id child-spec-id)
          (start-thunk child-spec-start-thunk)
          (restart child-spec-restart)
          (shutdown child-spec-shutdown)
          (child-type child-spec-type))
    "#,
    )
    .expect("child-spec record-type should define");

    let v = rt
        .eval_str(
            "<t>",
            r#"
        (define s (make-child-spec 'worker-1 (lambda () 'noop) 'permanent 5000 'worker))
        (list (child-spec? s)
              (child-spec-id s)
              (child-spec-restart s)
              (child-spec-shutdown s)
              (child-spec-type s))
    "#,
        )
        .expect("construct + access");
    assert_eq!(
        rt.format_value(&v, cs_core::WriteMode::Display),
        "(#t worker-1 permanent 5000 worker)"
    );
}

/// The supervisor helper `(prune-old timestamps period-seconds)`
/// — a pure-functional list manipulator that doesn't depend on
/// any actor primops. Verifies the helper expansions compile.
#[test]
fn prune_old_helper_definition_loads() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define current-jiffy (lambda () 1000))
        (define jiffies-per-second (lambda () 1))
        (define (prune-old timestamps period-seconds)
          (let* ((now (current-jiffy))
                 (cutoff (- now (* period-seconds (jiffies-per-second)))))
            (filter (lambda (t) (> t cutoff)) timestamps)))
    "#,
    )
    .expect("prune-old helper");

    // now=1000, period=10 -> cutoff=990. Keep timestamps > 990.
    let v = rt
        .eval_str("<t>", "(prune-old '(800 950 995 1000) 10)")
        .expect("prune-old invocation");
    assert_eq!(
        rt.format_value(&v, cs_core::WriteMode::Display),
        "(995 1000)"
    );
}

/// The actual `lib/beam/prelude.scm` file. Per #109 the prelude
/// was rewritten to use case-lambda + bare-symbol clause keys
/// instead of Racket `#:keyword` args, so the load now succeeds.
/// Spot-check that a representative subset of prelude bindings
/// (helpers + the supervisor record type) are procedure-bound
/// after loading.
#[test]
fn load_full_prelude_file_succeeds() {
    let prelude_path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/beam/prelude.scm");

    let src = std::fs::read_to_string(&prelude_path)
        .unwrap_or_else(|e| panic!("read {:?}: {}", prelude_path, e));

    let mut rt = Runtime::new();
    rt.eval_str("<prelude>", &src)
        .expect("lib/beam/prelude.scm should load after #109");

    // Spot-check: a handful of well-known prelude bindings should
    // be procedures after loading. Picking helpers that don't
    // require actor primops at lookup time.
    for name in &[
        "prune-old",
        "id-of-pid",
        "find-spec",
        "make-supervisor",
        "make-child-spec",
        "child-spec?",
    ] {
        let probe = format!("(procedure? {})", name);
        let v = rt
            .eval_str("<t>", &probe)
            .expect("procedure?-probe should at least eval");
        let s = rt.format_value(&v, cs_core::WriteMode::Display);
        assert_eq!(s, "#t", "{} should be defined as a procedure", name);
    }
}
