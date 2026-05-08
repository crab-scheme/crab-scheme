(test-section "do loops and guard form")

; Basic do loop
(test-eqv "do-sum-0-9"
  45
  (do ((i 0 (+ i 1)) (sum 0 (+ sum i)))
      ((= i 10) sum)))

(test-eqv "do-factorial-5"
  120
  (do ((i 1 (+ i 1)) (acc 1 (* acc i)))
      ((> i 5) acc)))

; do with empty body
(test-eqv "do-counter"
  10
  (do ((i 0 (+ i 1))) ((= i 10) i)))

; do with no result expression
(test-eqv "do-no-result"
  100
  (do ((i 0 (+ i 1)) (s 0 (+ s 10)))
      ((= i 10) s)))

; guard catching numeric condition
(test-eqv "guard-catches"
  42
  (guard (c ((number? c) (* c 2)))
    (raise 21)))

; guard with else
(test-eqv "guard-else"
  'fallback
  (guard (c ((symbol? c) 'a-symbol)
            (else 'fallback))
    (raise 42)))

; guard - no raise, returns body value
(test-eqv "guard-no-raise"
  100
  (guard (c (#t 'caught))
    100))

; guard with structured condition from error
(test-true "guard-error-cond"
  (guard (c ((condition? c) #t))
    (error "boom")))

; Nested guards: inner catches first
(test-eqv "nested-guards-inner"
  'inner
  (guard (c (#t 'outer))
    (guard (c (#t 'inner))
      (raise 'oops))))

; guard re-raises if no clause matches and no else
(test-eqv "guard-rethrow"
  'caught-by-outer
  (guard (c (#t 'caught-by-outer))
    (guard (c ((string? c) 'inner))    ; inner doesn't match number
      (raise 99))))
