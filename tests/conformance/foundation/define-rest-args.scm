; Regression: the (define (f . rest) …) / (define (f a . rest) …)
; function-definition shorthand with a dotted rest tail. Previously the
; reader rejected these as "define: bad function form" because the
; shorthand only accepted proper formals; it now reuses the same
; rest-aware parser as `lambda`.

(test-section "define with a bare rest list")
(define (all-args . xs) xs)
(test-equal "(define (f . xs)) collects every argument" '(1 2 3) (all-args 1 2 3))
(test-equal "(define (f . xs)) with no arguments" '() (all-args))

(test-section "define with fixed args + a rest tail")
(define (first-rest a . xs) (list a xs))
(test-equal "(define (f a . xs)) splits fixed from rest" '(1 (2 3)) (first-rest 1 2 3))
(test-equal "(define (f a . xs)) with only the fixed arg" '(1 ()) (first-rest 1))

(define (two-rest a b . xs) (list a b xs))
(test-equal "(define (f a b . xs))" '(1 2 (3 4 5)) (two-rest 1 2 3 4 5))

(test-section "rest-arg define still supports a full body")
(define (with-body . xs)
  (define n (length xs))
  (* n 2))
(test-equal "internal define + trailing expression" 6 (with-body 'a 'b 'c))

; Plain fixed-arity shorthand must keep working.
(define (plain a b) (+ a b))
(test-equal "fixed-arity shorthand unaffected" 7 (plain 3 4))
