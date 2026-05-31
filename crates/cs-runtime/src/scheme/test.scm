;;; (crab test) — a small unit-test framework.
;;;
;;; A bundled Scheme library, evaluated into the global environment at
;;; startup (so `(import (crab test))` is a no-op). The answer to
;;; Python's unittest/pytest, Go's testing, and clojure.test.
;;;
;;; Define tests with `deftest`, assert inside them, then `(run-tests)`.
;;; Assertions raise on failure; `run-tests` catches per-test, tallies
;;; pass/fail, prints a line per test, and returns (list passed failed).

;; Registry of (name . thunk), most-recently-defined first.
(define *crab-tests* '())

;; Forget all registered tests (call before a fresh run).
(define (clear-tests!)
  (set! *crab-tests* '()))

(define (register-test! name thunk)
  (set! *crab-tests* (cons (cons name thunk) *crab-tests*)))

;; (deftest name body …) registers a test that runs `body …`.
(define-syntax deftest
  (syntax-rules ()
    ((_ name body ...)
     (register-test! 'name (lambda () body ...)))))

;; --- assertions (raise on failure) ---

(define (assert-true x)
  (unless x (error "assert-true failed" x)))

(define (assert-false x)
  (when x (error "assert-false failed" x)))

(define (assert-equal expected actual)
  (unless (equal? expected actual)
    (error "assert-equal failed" expected actual)))

(define (assert-eqv expected actual)
  (unless (eqv? expected actual)
    (error "assert-eqv failed" expected actual)))

;; (assert-raises expr) passes iff evaluating `expr` raises.
(define-syntax assert-raises
  (syntax-rules ()
    ((_ expr)
     (let ((raised #f))
       (guard (e (#t (set! raised #t))) expr)
       (unless raised
         (error "assert-raises: expected an error but none was raised"))))))

;; --- runner ---

;; Run one thunk; return #t if it completed without raising.
(define (run-one-test thunk)
  (guard (e (#t #f)) (thunk) #t))

;; Run every registered test (in definition order), printing a line
;; each, and return (list passed failed).
(define (run-tests)
  (let loop ((tests (reverse *crab-tests*)) (pass 0) (fail 0))
    (cond
      ((null? tests)
       (display "tests: ") (display pass) (display " passed, ")
       (display fail) (display " failed") (newline)
       (list pass fail))
      (else
       (let* ((name (caar tests))
              (ok (run-one-test (cdar tests))))
         (display (if ok "  ok   " "  FAIL ")) (display name) (newline)
         (loop (cdr tests)
               (if ok (+ pass 1) pass)
               (if ok fail (+ fail 1))))))))
