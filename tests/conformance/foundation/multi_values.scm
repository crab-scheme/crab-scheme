(test-section "Multiple values: values / call-with-values")

; values returning a single value passes through normally
(test-eqv "values-single" 42 (values 42))

; call-with-values with single value
(test-eqv "cwv-single"
  100
  (call-with-values (lambda () 100) (lambda (x) x)))

; call-with-values with multiple values
(test-eqv "cwv-multi-sum"
  6
  (call-with-values (lambda () (values 1 2 3)) +))

(test-eqv "cwv-multi-list-len"
  3
  (call-with-values
    (lambda () (values 'a 'b 'c))
    (lambda (a b c) 3)))

; nested call-with-values
(test-eqv "cwv-nested"
  10
  (call-with-values
    (lambda () (values 4 6))
    (lambda (a b)
      (call-with-values
        (lambda () (values a b))
        +))))

; values in conditional
(test-eqv "cwv-after-cond"
  20
  (call-with-values
    (lambda ()
      (if #t (values 5 15) (values 1 2)))
    +))
