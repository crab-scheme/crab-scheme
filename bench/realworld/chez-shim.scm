;; Chez shim for the realworld bench wrappers.
;;
;; Provides a (realworld-bench name params thunk) that just runs the
;; thunk once and writes the result so cross-impl correctness checks
;; can hash it. No timing — bench/realworld/runner.sh handles that
;; for CrabScheme via the Phase B GC primops; Chez is used purely as
;; a reference answer for the (check-result-vs-chez.sh) gate.
;;
;; The bench files use (write) on values; if a bench's result is a
;; list of chars (e.g., maze), it ends up serialized the same way on
;; both Chez and CrabScheme.

;; Bridges Chez's default scope to the small subset of R7RS-or-R6RS
;; predicates / numeric ops the bench bodies expect. Add aliases
;; as new benches surface more gaps.
(import (chezscheme))
(define (exact-integer? x)
  (and (integer? x) (exact? x)))
(define arithmetic-shift
  (lambda (n s)
    (if (negative? s)
        (bitwise-arithmetic-shift-right n (- s))
        (bitwise-arithmetic-shift-left n s))))

(define (realworld-bench name params thunk)
  (let ((result (thunk)))
    (write result)
    (newline)))
