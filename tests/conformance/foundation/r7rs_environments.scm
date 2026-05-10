(test-section "R7RS legacy environments: null-environment, scheme-report-environment")

; --- null-environment requires version 5 ---
(define ne (null-environment 5))
(test-true "ne-returns-something" (not (eq? ne #f)))

; --- null-environment rejects other versions ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (null-environment 7))))))
(test-eqv "ne-bad-version" 'caught c1)

; --- scheme-report-environment accepts 5 and 7 ---
(define sre5 (scheme-report-environment 5))
(define sre7 (scheme-report-environment 7))
(test-true "sre5-returns" (not (eq? sre5 #f)))
(test-true "sre7-returns" (not (eq? sre7 #f)))

; --- scheme-report-environment rejects other versions ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (scheme-report-environment 6))))))
(test-eqv "sre-bad-version" 'caught c2)

; --- arity errors ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (null-environment))))))
(test-eqv "ne-arity-0" 'caught c3)

(define c4
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (scheme-report-environment 5 6))))))
(test-eqv "sre-arity-2" 'caught c4)

; --- eval works against scheme-report-environment ---
(test-eqv "eval-with-sre"
  42
  (eval '(+ 21 21) (scheme-report-environment 7)))

; --- eval with default current environment also works ---
(test-eqv "eval-default-env"
  100
  (eval '(* 10 10) (interaction-environment)))

; --- environment-like symbols are symbols ---
(test-true "env-is-symbol" (symbol? (interaction-environment)))
(test-true "sre-is-symbol" (symbol? (scheme-report-environment 7)))
(test-true "ne-is-symbol"  (symbol? (null-environment 5)))
