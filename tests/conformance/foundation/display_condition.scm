(test-section "display-condition")

; display-condition writes to a string output port for inspection.
(define (rendered c)
  (let ((p (open-string-output-port)))
    (display-condition c p)
    (get-output-string p)))

; error with who
(define c1
  (with-exception-handler (lambda (c) c)
    (lambda () (error 'my-fn "bad input" 42 'oops))))
(test-equal "display-error-who"
  "error in my-fn: bad input (42 oops)\n"
  (rendered c1))

; error without who
(define c2
  (with-exception-handler (lambda (c) c)
    (lambda () (error "no who" 1 2))))
(test-equal "display-error-no-who"
  "error: no who (1 2)\n"
  (rendered c2))

; assertion-violation uses the assertion-violation prefix
(define c3
  (with-exception-handler (lambda (c) c)
    (lambda () (assertion-violation 'check "broke" -5))))
(test-equal "display-av"
  "assertion-violation in check: broke (-5)\n"
  (rendered c3))

; #f who is suppressed
(define c4
  (with-exception-handler (lambda (c) c)
    (lambda () (error #f "anonymous"))))
(test-equal "display-error-false-who"
  "error: anonymous\n"
  (rendered c4))

; user-defined condition type uses its tag as a [&type] suffix.
; Because there's no &message simple, the message slot after `error:`
; stays empty and the suffix is what the user sees.
(define-condition-type &custom &error make-custom custom?)
(define cc (make-custom))
(test-equal "display-user-cond"
  "error: [&custom]\n"
  (rendered cc))

; non-condition raises an error itself.
(test-true "display-condition-rejects-non-cond"
  (with-exception-handler
    (lambda (c) #t)
    (lambda () (display-condition 42))))
