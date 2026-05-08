(test-section "named let + assert")

; Named let: classic countdown sum.
(test-eqv "named-let-sum"
  55
  (let loop ((i 0) (acc 0))
    (if (> i 10) acc (loop (+ i 1) (+ acc i)))))

; Named let: build a list backwards.
(test-equal "named-let-list"
  '(1 2 3 4 5)
  (let loop ((i 5) (acc '()))
    (if (= i 0) acc (loop (- i 1) (cons i acc)))))

; Named let: factorial.
(test-eqv "named-let-fact"
  120
  (let fact ((n 5))
    (if (= n 0) 1 (* n (fact (- n 1))))))

; Plain (non-named) let still works.
(test-eqv "plain-let"
  7
  (let ((x 3) (y 4)) (+ x y)))

; assert with truthy expression: returns unspecified, no error.
(test-true "assert-truthy"
  (begin (assert (= 2 2)) #t))

; assert raises an error condition on falsy expression. We catch via
; with-exception-handler so the test harness can verify.
(test-true "assert-falsy-raises"
  (with-exception-handler
    (lambda (c) (condition? c))
    (lambda () (assert (= 1 2)) #f)))
