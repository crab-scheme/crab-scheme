//! Regression test for issue #19 — JIT silent miscompile on
//! cross-function calls.
//!
//! The legacy pure-fixnum JIT tier silently miscompiled a body that
//! makes a cross-function call (`Inst::CallGeneral`): the call clobbered
//! state so a following `Car`/`Cdr`/`Cons`/`CallSelf` read garbage,
//! returning `Null` where the caller expected a pair. nboyer/sboyer hit
//! this at `(caddr (car lst))`; the minimal shape is a recursive map
//! that calls a *sibling* function — pre-fix it returned only the first
//! element of the list. The fix keeps such bodies off the pure-fixnum
//! tier (routing to the VM, which is always correct).
//!
//! The VM/walker tiers always evaluated these correctly, so the
//! expected values double as a differential oracle.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn jit_eval(src: &str) -> String {
    let mut rt = Runtime::new();
    rt.install_jit().expect("install_jit");
    // eval_str_via_vm tiers hot bodies up to the JIT — the path #19 hit.
    let v = rt
        .eval_str_via_vm("<i19>", src)
        .unwrap_or_else(|d| panic!("eval failed: {}", d.message));
    rt.format_value(&v, WriteMode::Display)
}

#[test]
fn recursive_map_with_cross_function_call_keeps_whole_list() {
    // `mp` maps `idf` (a sibling — CallGeneral) over a list, plus a Cons
    // and a CallSelf. Pre-fix the JIT dropped every element past the
    // first: `(1)` instead of `(1 2 3 4 5)`.
    let out = jit_eval(
        "(define (idf x) x)
         (define (mp lst)
           (if (null? lst) '() (cons (idf (car lst)) (mp (cdr lst)))))
         (let loop ((i 0)) (when (< i 5000) (mp '(1 2 3 4 5)) (loop (+ i 1))))
         (mp '(1 2 3 4 5))",
    );
    assert_eq!(out, "(1 2 3 4 5)");
}

#[test]
fn nboyer_rewriter_shape_keeps_whole_term() {
    // The nboyer/sboyer rewriter shape: a 2-arg cross-function call whose
    // first arg is `(cons (car term) (recur (cdr term)))`. Pre-fix the
    // rebuilt term lost its tail.
    let out = jit_eval(
        "(define (rw-args lst)
           (if (null? lst) '() (cons (rw (car lst)) (rw-args (cdr lst)))))
         (define (keep a b) a)
         (define (rw term)
           (cond ((not (pair? term)) term)
                 (else (keep (cons (car term) (rw-args (cdr term))) (car term)))))
         (let loop ((i 0)) (when (< i 5000) (rw '(1 (2 3) 4 5)) (loop (+ i 1))))
         (rw '(1 (2 3) 4 5))",
    );
    assert_eq!(out, "(1 (2 3) 4 5)");
}
