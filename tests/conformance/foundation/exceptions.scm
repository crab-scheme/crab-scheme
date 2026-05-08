(test-section "Exceptions: raise / error / with-exception-handler")

; raise + with-exception-handler
(test-eqv "wrap-handler-receives-value"
  'caught
  (with-exception-handler
    (lambda (cond) 'caught)
    (lambda () (raise 42))))

; handler can return a useful value substituting for the body's result
(test-eqv "handler-returns-value"
  99
  (with-exception-handler
    (lambda (cond) 99)
    (lambda () (raise 'oops))))

; If thunk returns normally, with-exception-handler returns that value
(test-eqv "thunk-no-raise" 42
  (with-exception-handler
    (lambda (c) 'unused)
    (lambda () 42)))

; error builtin produces a condition
(test-true "error-makes-condition"
  (with-exception-handler
    (lambda (c) (condition? c))
    (lambda () (error "boom"))))

; condition? on non-conditions
(test-false "condition-on-int"     (condition? 42))
(test-false "condition-on-symbol"  (condition? 'foo))
(test-false "condition-on-list"    (condition? '(1 2 3)))

; Nested handlers
(test-eqv "nested-handlers"
  'inner
  (with-exception-handler
    (lambda (c) 'outer)
    (lambda ()
      (with-exception-handler
        (lambda (c) 'inner)
        (lambda () (raise 'oops))))))
