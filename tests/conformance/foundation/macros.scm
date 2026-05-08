(test-section "syntax-rules macros (M3 first cut, no hygiene)")

; Simple no-arg macro
(define-syntax always-42
  (syntax-rules ()
    ((_) 42)))
(test-eqv "macro-no-args" 42 (always-42))

; Single argument
(define-syntax square-it
  (syntax-rules ()
    ((_ x) (* x x))))
(test-eqv "macro-one-arg" 49 (square-it 7))
(test-eqv "macro-eval-arg-once-ok" 16 (square-it 4))   ; note: x is evaluated twice — known limitation

; Body ellipsis
(define-syntax my-when
  (syntax-rules ()
    ((_ test body ...) (if test (begin body ...) #f))))
(test-eqv "macro-when-true"  3  (my-when #t 1 2 3))
(test-eqv "macro-when-false" #f (my-when #f 1 2 3))

; my-unless
(define-syntax my-unless
  (syntax-rules ()
    ((_ test body ...) (if test #f (begin body ...)))))
(test-eqv "macro-unless-false" 100 (my-unless #f 1 2 100))
(test-eqv "macro-unless-true"  #f  (my-unless #t 1 2 3))

; Recursive macro: my-or with multiple rules
(define-syntax my-or
  (syntax-rules ()
    ((_) #f)
    ((_ e) e)
    ((_ e1 e2 ...) (if e1 e1 (my-or e2 ...)))))
(test-eqv   "my-or-empty"        #f  (my-or))
(test-eqv   "my-or-single"       7   (my-or 7))
(test-eqv   "my-or-first-truthy" 1   (my-or 1 2 3))
(test-eqv   "my-or-find-truthy"  42  (my-or #f #f 42 99))
(test-false "my-or-all-false"    (my-or #f #f #f))

; Mutation macro
(define-syntax incr!
  (syntax-rules ()
    ((_ x) (set! x (+ x 1)))))
(define cnt 0)
(incr! cnt)
(incr! cnt)
(incr! cnt)
(test-eqv "macro-incr-3-times" 3 cnt)

; Pattern with multiple ellipsis args
(define-syntax sum-all
  (syntax-rules ()
    ((_ x ...) (+ x ...))))
(test-eqv "sum-all-empty" 0 (sum-all))
(test-eqv "sum-all-one"   5 (sum-all 5))
(test-eqv "sum-all-many"  15 (sum-all 1 2 3 4 5))

; Pattern with literal keyword
(define-syntax my-loop
  (syntax-rules (until)
    ((_ until test body ...) (if test #f (begin body ... (my-loop until test body ...))))))
; Just ensures the literal keyword form parses; recursion limit prevents
; testing actual loop, but we can verify single-pass.
; This will infinite-loop if test is always false; just test the structure.
; Test: a finite version using a counter outside.
(define lp-counter 0)
(define-syntax noop-loop-3
  (syntax-rules ()
    ((_ body ...) (begin body ... body ... body ...))))
(noop-loop-3 (set! lp-counter (+ lp-counter 1)))
(test-eqv "macro-body-repeated-3" 3 lp-counter)

; Multi-clause: destructuring
(define-syntax my-cond
  (syntax-rules (else)
    ((_ (else e ...)) (begin e ...))
    ((_ (test e ...)) (if test (begin e ...) #f))
    ((_ (test e ...) clause ...) (if test (begin e ...) (my-cond clause ...)))))
(test-eqv "my-cond-else"
  100
  (my-cond (else 100)))
(test-eqv "my-cond-first"
  'first
  (my-cond (#t 'first) (#t 'second) (else 'fallback)))
(test-eqv "my-cond-fallback"
  'fallback
  (my-cond (#f 'first) (#f 'second) (else 'fallback)))
