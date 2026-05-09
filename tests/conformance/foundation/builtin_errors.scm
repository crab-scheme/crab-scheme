(test-section "builtin errors are catchable conditions")

; Type errors raise a proper condition. The handler can introspect via
; the standard R6RS predicates and accessors, including the offending
; value as an &irritants simple.
(define c1
  (with-exception-handler (lambda (c) c)
    (lambda () (+ 1 'foo))))
(test-true  "+-bad-arg-cond?"     (condition? c1))
(test-true  "+-bad-arg-error?"    (error? c1))
(test-true  "+-bad-arg-who?"      (who-condition? c1))
(test-equal "+-bad-arg-who"       '+ (condition-who c1))
(test-true  "+-bad-arg-msg?"      (message-condition? c1))
(test-true  "+-bad-arg-irritants?" (irritants-condition? c1))
(test-equal "+-bad-arg-irritants" '(foo) (condition-irritants c1))

; Comparisons
(define c2
  (with-exception-handler (lambda (c) c)
    (lambda () (< 1 "string"))))
(test-true  "<-bad-arg-error?"    (error? c2))
(test-equal "<-bad-arg-who"       '< (condition-who c2))

; List ops
(define c3
  (with-exception-handler (lambda (c) c)
    (lambda () (car 42))))
(test-true  "car-bad-arg-error?"  (error? c3))
(test-equal "car-bad-arg-who"     'car (condition-who c3))

(define c4
  (with-exception-handler (lambda (c) c)
    (lambda () (cdr '()))))
(test-true  "cdr-bad-arg-error?"  (error? c4))

; Division by zero
(define c5
  (with-exception-handler (lambda (c) c)
    (lambda () (/ 1 0))))
(test-true  "div0-error?"         (error? c5))
(test-true  "div0-msg?"           (message-condition? c5))

; Vector out-of-bounds
(define c6
  (with-exception-handler (lambda (c) c)
    (lambda () (vector-ref #(a b c) 99))))
(test-true  "vref-error?"         (error? c6))
(test-equal "vref-who"            'vector-ref (condition-who c6))

; Successful path: handler isn't invoked at all when the body returns
; normally — verify the new dispatch hasn't broken happy-path semantics.
(test-eqv "happy-path" 42
  (with-exception-handler (lambda (c) 'unused) (lambda () (+ 40 2))))

; Re-raising from a handler propagates to an outer handler.
(test-equal "rethrow-via-handler" 'outer
  (with-exception-handler
    (lambda (outer-c) 'outer)
    (lambda ()
      (with-exception-handler
        (lambda (c) (raise c))
        (lambda () (+ 1 'foo))))))
