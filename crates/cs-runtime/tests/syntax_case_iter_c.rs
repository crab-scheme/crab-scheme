//! R6RS++ §12 (#118) Iter C — with-syntax + quasisyntax.
//!
//! Tests:
//! * `with-syntax` desugars to nested syntax-case forms.
//! * `quasisyntax` / `unsyntax` / `unsyntax-splicing` work as
//!   syntax-flavored quasiquote (same semantics today since we
//!   don't track marks).
//! * `unsyntax` outside `quasisyntax` is a syntax error.
//!
//! Ellipsis in patterns/templates is split into a follow-up
//! iter (Iter C2 / Iter F); not exercised here.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- with-syntax ----

#[test]
fn with_syntax_single_binding() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-syntax ((x 42))
               (syntax x))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn with_syntax_multiple_bindings_left_to_right() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-syntax ((a 1) (b 2))
               (list (syntax a) (syntax b)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 2)");
}

#[test]
fn with_syntax_destructuring_binding() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-syntax (((a b c) '(x y z)))
               (list (syntax c) (syntax a)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(z x)");
}

#[test]
fn with_syntax_body_sees_pvars_as_scheme_vars() {
    // Inside with-syntax body, pvars are plain Scheme variables;
    // they can be used directly without `(syntax ...)`.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-syntax ((a 10) (b 20))
               (+ a b))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "30");
}

#[test]
fn with_syntax_multi_form_body() {
    // `body ...` is a sequence; only the last value is returned.
    // Use a side effect to verify earlier forms execute.
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define counter 0)").unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "(with-syntax ((x 5))
               (set! counter (+ counter x))
               (set! counter (+ counter x))
               counter)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "10");
}

#[test]
fn with_syntax_empty_bindings() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(with-syntax () (+ 1 2))").unwrap();
    assert_eq!(disp(&rt, &v), "3");
}

#[test]
fn with_syntax_rejects_malformed_binding() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(with-syntax ((x)) 0)")
        .expect_err("missing value should fail");
    let s = format!("{}", err);
    assert!(s.contains("with-syntax"), "got: {}", s);
}

// ---- quasisyntax / unsyntax ----

#[test]
fn quasisyntax_atom_is_identity() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(quasisyntax foo)").unwrap();
    assert_eq!(disp(&rt, &v), "foo");
    let v = rt.eval_str("<t>", "(quasisyntax 42)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn quasisyntax_list_is_identity() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(quasisyntax (a b c))").unwrap();
    assert_eq!(disp(&rt, &v), "(a b c)");
}

#[test]
fn quasisyntax_unsyntax_interpolates() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((x 42))
               (quasisyntax (the-answer (unsyntax x))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(the-answer 42)");
}

#[test]
fn quasisyntax_unsyntax_splicing() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((xs '(1 2 3)))
               (quasisyntax (head (unsyntax-splicing xs) tail)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(head 1 2 3 tail)");
}

#[test]
fn quasisyntax_inside_with_syntax_uses_pvars() {
    // pvars from with-syntax are normal vars at runtime, so
    // (unsyntax pvar) interpolates their value.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-syntax ((id 'my-name))
               (quasisyntax (define (unsyntax id) 42)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(define my-name 42)");
}

#[test]
fn quasisyntax_nested() {
    // Outer quasisyntax at depth 1; inner unsyntax pops to 0 so
    // its arg evaluates.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((x 7))
               (quasisyntax (a (unsyntax x) b)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(a 7 b)");
}

#[test]
fn unsyntax_outside_quasisyntax_errors() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(unsyntax 42)")
        .expect_err("unsyntax outside quasisyntax should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("unsyntax") || s.contains("quasisyntax"),
        "got: {}",
        s
    );
}

#[test]
fn unsyntax_splicing_outside_quasisyntax_errors() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(unsyntax-splicing '(1 2))")
        .expect_err("unsyntax-splicing outside quasisyntax should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("unsyntax") || s.contains("quasisyntax"),
        "got: {}",
        s
    );
}

// ---- composition: with-syntax + quasisyntax + syntax-case ----

#[test]
fn macro_writer_pipeline_simulation() {
    // Simulates a tiny "swap-1st-2nd" macro built from the
    // syntax-case + with-syntax + quasisyntax stack.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(a b c d) ()
               ((x y . rest)
                (with-syntax ((newx (syntax y))
                              (newy (syntax x)))
                  (quasisyntax ((unsyntax newx)
                                (unsyntax newy)
                                (unsyntax-splicing rest))))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(b a c d)");
}
