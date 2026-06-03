;;; (crab expect) — Gomega-style matchers and assertions.
;;;
;;; A bundled Scheme library (global at startup; `(import (crab expect))`
;;; is a no-op). The answer to Gomega. A matcher is a procedure
;;; `(matcher actual) -> (cons pass? message)`; `expect` raises on
;;; failure (the `(crab spec)` runner catches it), `expect-not` inverts.

(define (expect:->str x)
  (let ((p (open-output-string)))
    (write x p)
    (get-output-string p)))

(define (expect:fail msg)
  (error (string-append "expectation failed: " msg)))

;; (expect actual matcher) — assert the matcher passes.
(define (expect actual matcher)
  (let ((r (matcher actual)))
    (if (car r) #t (expect:fail (cdr r)))))

;; (expect-not actual matcher) — assert the matcher fails.
(define (expect-not actual matcher)
  (let ((r (matcher actual)))
    (if (car r)
        (expect:fail (string-append "(negated) " (cdr r)))
        #t)))

;; ---- equality / identity ----

(define (equal expected)
  (lambda (a)
    (cons (equal? a expected)
          (string-append "expected " (expect:->str a)
                         " to equal " (expect:->str expected)))))

(define (be expected)
  (lambda (a)
    (cons (eqv? a expected)
          (string-append "expected " (expect:->str a)
                         " to be " (expect:->str expected)))))

;; ---- booleans / nil ----

(define (be-true)
  (lambda (a) (cons (eq? a #t) (string-append "expected " (expect:->str a) " to be #t"))))
(define (be-false)
  (lambda (a) (cons (eq? a #f) (string-append "expected " (expect:->str a) " to be #f"))))
(define (be-truthy)
  (lambda (a) (cons (if a #t #f) (string-append "expected " (expect:->str a) " to be truthy"))))
(define (be-falsy)
  (lambda (a) (cons (not a) (string-append "expected " (expect:->str a) " to be falsy"))))
(define (be-nil)
  (lambda (a) (cons (null? a) (string-append "expected " (expect:->str a) " to be ()"))))

;; ---- collections ----

(define (contain x)
  (lambda (a)
    (cons (and (list? a) (if (member x a) #t #f))
          (string-append "expected " (expect:->str a) " to contain " (expect:->str x)))))

(define (have-len n)
  (lambda (a)
    (let ((len (cond ((string? a) (string-length a))
                     ((vector? a) (vector-length a))
                     ((list? a) (length a))
                     (else -1))))
      (cons (= len n)
            (string-append "expected " (expect:->str a)
                           " to have length " (number->string n)
                           " (got " (number->string len) ")")))))

(define (be-empty)
  (lambda (a)
    (let ((empty (cond ((string? a) (= 0 (string-length a)))
                       ((vector? a) (= 0 (vector-length a)))
                       ((list? a) (null? a))
                       (else #f))))
      (cons empty (string-append "expected " (expect:->str a) " to be empty")))))

;; ---- numeric ----

(define (be-> n)
  (lambda (a) (cons (> a n) (string-append "expected " (expect:->str a) " > " (number->string n)))))
(define (be->= n)
  (lambda (a) (cons (>= a n) (string-append "expected " (expect:->str a) " >= " (number->string n)))))
(define (be-< n)
  (lambda (a) (cons (< a n) (string-append "expected " (expect:->str a) " < " (number->string n)))))
(define (be-<= n)
  (lambda (a) (cons (<= a n) (string-append "expected " (expect:->str a) " <= " (number->string n)))))
(define (be-close-to x tol)
  (lambda (a)
    (cons (<= (abs (- a x)) tol)
          (string-append "expected " (expect:->str a)
                         " to be within " (number->string tol) " of " (number->string x)))))

;; ---- predicate / string ----

(define (satisfy pred)
  (lambda (a) (cons (if (pred a) #t #f)
                    (string-append "expected " (expect:->str a) " to satisfy the predicate"))))

(define (contain-substring sub)
  (lambda (a)
    (cons (and (string? a) (expect:str-contains? a sub))
          (string-append "expected " (expect:->str a)
                         " to contain substring " (expect:->str sub)))))

(define (expect:str-contains? hay needle)
  (let ((hn (string-length hay)) (nn (string-length needle)))
    (let loop ((i 0))
      (cond ((> (+ i nn) hn) #f)
            ((string=? (substring hay i (+ i nn)) needle) #t)
            (else (loop (+ i 1)))))))

;; ---- raising ----

;; (expect-raise thunk) — assert that calling `thunk` raises.
(define (expect-raise thunk)
  (let ((raised #f))
    (guard (e (#t (set! raised #t))) (thunk))
    (if raised #t (expect:fail "expected an error to be raised"))))

;; (expect-no-raise thunk) — assert that calling `thunk` does not raise.
(define (expect-no-raise thunk)
  (guard (e (#t (expect:fail "expected no error, but one was raised")))
    (thunk)
    #t))

;; ---- async (Gomega Eventually / Consistently) ----

;; (eventually thunk matcher [timeout-ms [interval-ms]]) — re-run `thunk`
;; until the matcher passes, or fail after the timeout.
(define (eventually thunk matcher . opts)
  (let ((timeout (if (pair? opts) (car opts) 1000))
        (interval (if (and (pair? opts) (pair? (cdr opts))) (cadr opts) 10)))
    (let ((deadline (+ (current-time-ms) timeout)))
      (let loop ()
        (let ((r (matcher (thunk))))
          (cond ((car r) #t)
                ((>= (current-time-ms) deadline)
                 (expect:fail (string-append "eventually: " (cdr r))))
                (else (sleep-ms interval) (loop))))))))

;; (consistently thunk matcher [duration-ms [interval-ms]]) — the matcher
;; must hold every time it is checked over the duration.
(define (consistently thunk matcher . opts)
  (let ((duration (if (pair? opts) (car opts) 200))
        (interval (if (and (pair? opts) (pair? (cdr opts))) (cadr opts) 10)))
    (let ((deadline (+ (current-time-ms) duration)))
      (let loop ()
        (let ((r (matcher (thunk))))
          (cond ((not (car r))
                 (expect:fail (string-append "consistently: " (cdr r))))
                ((>= (current-time-ms) deadline) #t)
                (else (sleep-ms interval) (loop))))))))
