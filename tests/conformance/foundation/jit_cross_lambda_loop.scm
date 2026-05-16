; Regression test for the cross-lambda Fixnum-return loop bug
; documented in docs/research/jit_loop_cross_lambda_bug.md.
;
; Pattern: a JIT'd loop body calls another lambda via CallGeneral,
; whose Fixnum return becomes the next iteration's accumulator.
;
; Pre-iter3: this produced garbage NB-encoded Gc<Value> values on
; --tier vm-jit (e.g. -140707035149248 instead of 5000).
; Iter3 (53207f2) inadvertently fixed it by adding BoxTyped support
; in uniform-NB, so the loop body no longer falls back to specialized
; tier with the broken return-type inference.
;
; Uses named-let (tail-call eliminated on walker) so the test can
; run on all three tiers without burning host stack.

(test-section "jit cross-lambda loop (regression)")

(define (inner-incr n) (+ n 1))

(define (named-let-form iters)
  (let loop ((count iters) (acc 0))
    (if (= count 0)
        acc
        (loop (- count 1) (inner-incr acc)))))

; Small N — bytecode path
(test-eqv "named-let-form(100)" 100 (named-let-form 100))
; Medium — tier-up boundary
(test-eqv "named-let-form(5000)" 5000 (named-let-form 5000))
; Large — well past tier-up, stable JIT
(test-eqv "named-let-form(50000)" 50000 (named-let-form 50000))

; Inner returns a Fixnum derived from arithmetic (not just +1)
; — exercises a different value path through the loop
(define (inner-mul2 n) (* n 2))
(define (mul-loop iters target)
  (let loop ((i 0) (acc 1))
    (if (= i iters)
        acc
        (loop (+ i 1) (if (>= acc target) acc (inner-mul2 acc))))))

(test-eqv "mul-loop saturates at 2^32" 4294967296 (mul-loop 50000 4294967296))
