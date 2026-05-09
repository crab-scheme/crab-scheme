(test-section "R6RS error/assertion-violation with `who` argument")

; --- error: R6RS form with leading symbol who ---
(define c1
  (with-exception-handler (lambda (c) c)
    (lambda () (error 'my-fn "bad input" 42 'oops))))
(test-true  "r6-error-cond?"      (condition? c1))
(test-true  "r6-error-error?"     (error? c1))
(test-true  "r6-error-who?"       (who-condition? c1))
(test-equal "r6-error-who"        'my-fn (condition-who c1))
(test-equal "r6-error-message"    "bad input" (condition-message c1))
(test-equal "r6-error-irritants"  '(42 oops) (condition-irritants c1))

; --- error: leading #f who ---
(define c2
  (with-exception-handler (lambda (c) c)
    (lambda () (error #f "anonymous error"))))
(test-true  "f-error-who?"        (who-condition? c2))
(test-false "f-error-who-val"     (condition-who c2))
(test-equal "f-error-message"     "anonymous error" (condition-message c2))

; --- error: leading string-who, then string-message (R6RS-style) ---
(define c3
  (with-exception-handler (lambda (c) c)
    (lambda () (error "my-component" "operation failed" 'detail))))
(test-true  "s-error-who?"        (who-condition? c3))
(test-equal "s-error-who"         "my-component" (condition-who c3))
(test-equal "s-error-message"     "operation failed" (condition-message c3))
(test-equal "s-error-irritants"   '(detail) (condition-irritants c3))

; --- error: R7RS-style: single string is the message, no who ---
(define c4
  (with-exception-handler (lambda (c) c)
    (lambda () (error "just a message"))))
(test-true  "r7-error-error?"     (error? c4))
(test-false "r7-error-no-who"     (who-condition? c4))
(test-equal "r7-error-message"    "just a message" (condition-message c4))

; --- error: R7RS-style with non-string irritants (no who) ---
(define c5
  (with-exception-handler (lambda (c) c)
    (lambda () (error "msg" 1 2 3))))
(test-false "r7-irr-no-who"       (who-condition? c5))
(test-equal "r7-irr-message"      "msg" (condition-message c5))
(test-equal "r7-irr-irritants"    '(1 2 3) (condition-irritants c5))

; --- assertion-violation: always takes (who msg ...) ---
(define av1
  (with-exception-handler (lambda (c) c)
    (lambda () (assertion-violation 'check-arg "expected positive" -5))))
(test-true  "av-pred"             (assertion-violation? av1))
(test-true  "av-violation"        (violation? av1))
(test-true  "av-serious"          (serious-condition? av1))
; R6RS distinguishes: assertion-violation is NOT an &error.
(test-false "av-not-error"        (error? av1))
(test-true  "av-who?"             (who-condition? av1))
(test-equal "av-who"              'check-arg (condition-who av1))
(test-equal "av-message"          "expected positive" (condition-message av1))
(test-equal "av-irritants"        '(-5) (condition-irritants av1))

; --- assertion-violation with no irritants ---
(define av2
  (with-exception-handler (lambda (c) c)
    (lambda () (assertion-violation #f "broken invariant"))))
(test-true  "av2-no-who-but-cond" (who-condition? av2))
(test-false "av2-no-who-val"      (condition-who av2))
(test-equal "av2-message"         "broken invariant" (condition-message av2))
(test-false "av2-no-irritants"    (irritants-condition? av2))
