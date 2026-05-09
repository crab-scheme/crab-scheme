(test-section "cond/guard with => and single-test clauses")

; --- cond => ---
; (cond (TEST => CONSUMER)) calls (CONSUMER TEST-VALUE) when TEST is truthy.
; Here TEST-VALUE is (+ 1 2) = 3, so CONSUMER returns (* 3 4) = 12.
(test-eqv "cond-arrow-basic"
  12
  (cond ((+ 1 2) => (lambda (n) (* n 4)))
        (else 99)))

; cond => threads test value (a pair from assv) into the consumer.
(test-equal "cond-arrow-assv"
  '(b 2)
  (cond ((assv 'b '((a 1) (b 2) (c 3))) => (lambda (p) p))
        (else '())))

; cond => with falsy test falls through to else.
(test-eqv "cond-arrow-falsy"
  'caught-else
  (cond ((assv 'z '((a 1))) => (lambda (p) p))
        (else 'caught-else)))

; --- cond single-test ---
(test-eqv "cond-test-only-truthy" 42 (cond (42) (else 0)))
; falsy single-test falls through.
(test-equal "cond-test-only-falsy" 'fallback
  (cond (#f) (else 'fallback)))

; --- cond multi-body still works ---
(test-eqv "cond-body-last-value"
  30
  (cond ((= 1 1) 10 20 30)
        (else 99)))

; --- guard => ---
; classic table-dispatch idiom: handlers mapped by raised symbol.
(define handlers
  '((net . "network failure")
    (io  . "io failure")))
(test-equal "guard-arrow-net"
  "network failure"
  (guard (c ((assq c handlers) => cdr)
            (else "unknown"))
    (raise 'net)))
(test-equal "guard-arrow-default"
  "unknown"
  (guard (c ((assq c handlers) => cdr)
            (else "unknown"))
    (raise 'wat)))

; guard with single-test returning test value
(test-eqv "guard-test-only"
  42
  (guard (c ((number? c) c)
            (else 'not-num))
    (raise 42)))

; guard with => in a chain followed by predicate body. The => clause
; receives the *test* value (#t from number?), not the raised condition.
(test-equal "guard-mixed-clauses"
  '(matched #t)
  (guard (c ((boolean? c) 'flagged)
            ((number? c) => (lambda (truthy) (list 'matched truthy)))
            (else 'other))
    (raise 7)))

; To use the raised value itself, write a normal-body clause that
; references the bound variable.
(test-equal "guard-body-references-var"
  '(matched 7)
  (guard (c ((boolean? c) 'flagged)
            ((number? c) (list 'matched c))
            (else 'other))
    (raise 7)))
