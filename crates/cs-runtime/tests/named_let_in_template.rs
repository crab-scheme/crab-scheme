//! Regression tests for #119 — named-let inside syntax-rules templates.
//!
//! `(let LOOP ((var init) ...) body)` is a named-let. When it
//! appears inside a syntax-rules template, the hygiene pass must
//! recognize the 4-arg `(let name bindings body...)` shape and
//! gensym-rename both the loop name AND the binding names. The
//! original implementation only handled the 3-arg
//! `(let bindings body...)` shape and silently dropped the
//! binding-name renames -- runtime then saw the binders as free
//! variables.
//!
//! Discovered while writing the BEAM `receive` macro
//! (`lib/beam/prelude.scm`); workaround was to rewrite via
//! `(letrec ((LOOP (lambda (var ...) body))) (LOOP init ...))`.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

#[test]
fn named_let_in_simple_template() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax count-to-3
          (syntax-rules ()
            ((_)
             (let loop ((x 0))
               (if (>= x 3) x (loop (+ x 1)))))))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(count-to-3)").unwrap();
    assert_eq!(disp(&rt, &v), "3");
}

#[test]
fn named_let_with_user_arg_substituted_into_body() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax sum-to-n
          (syntax-rules ()
            ((_ n)
             (let loop ((i 0) (acc 0))
               (if (>= i n)
                   acc
                   (loop (+ i 1) (+ acc i)))))))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(sum-to-n 5)").unwrap();
    // 0+1+2+3+4 = 10
    assert_eq!(disp(&rt, &v), "10");
}

#[test]
fn named_let_inside_let_in_template() {
    // Mix outer plain-let with inner named-let; both must
    // rename their bindings correctly without collision.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax fact-via-loop
          (syntax-rules ()
            ((_ n)
             (let ((m n))
               (let loop ((i 1) (acc 1))
                 (if (> i m)
                     acc
                     (loop (+ i 1) (* acc i))))))))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(fact-via-loop 5)").unwrap();
    assert_eq!(disp(&rt, &v), "120");
}

#[test]
fn named_let_loop_references_itself_recursively() {
    // The loop name itself must be renamed-and-shadowed so the
    // recursive call resolves to the let-bound version, not any
    // outer `loop` binding.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        ; Bind an outer `loop` variable; the named-let inside the
        ; macro should NOT capture it. With proper hygiene, the
        ; introduced `loop` gets renamed and the outer `loop` stays
        ; visible only to user code (not the macro).
        (define loop 'not-a-procedure)

        (define-syntax safe-count
          (syntax-rules ()
            ((_ n)
             (let loop ((i 0))
               (if (>= i n) i (loop (+ i 1)))))))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(safe-count 4)").unwrap();
    assert_eq!(disp(&rt, &v), "4");
}

#[test]
fn plain_let_in_template_still_works() {
    // Regression check: the named-let fix must not break the
    // plain-let path.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax bind-and-add
          (syntax-rules ()
            ((_ x y)
             (let ((a x) (b y)) (+ a b)))))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(bind-and-add 3 4)").unwrap();
    assert_eq!(disp(&rt, &v), "7");
}
