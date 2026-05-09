(test-section "R7RS delay-force + iterative force")

; --- delay-force is a syntactic form ---
(test-true "delay-force-makes-promise" (promise? (delay-force 42)))

; --- forcing delay-force returns the inner value ---
(test-eqv "delay-force-of-value" 42 (force (delay-force 42)))

; --- delay-force wrapping delay (one chain step) ---
(test-eqv "delay-force-of-delay" 7 (force (delay-force (delay 7))))

; --- delay-force iterates: thunk returning a promise ---
; A two-level chain: delay-force returns delay returns 100.
(test-eqv "delay-force-2-levels" 100
  (force (delay-force (delay (delay 100)))))

; --- delay-force iterates many levels (proves iterative force, not recursive)
; Without the iterative force loop, this would blow the host stack.
(define (make-chain n v)
  (if (= n 0)
      (delay v)
      (delay-force (make-chain (- n 1) v))))
(test-eqv "delay-force-1000" 'deep
  (force (make-chain 1000 'deep)))

; --- force is idempotent: forcing the same promise twice returns the
;     same value (memoized) ---
(define p (delay (begin 99)))
(test-eqv "force-1st" 99 (force p))
(test-eqv "force-2nd" 99 (force p))

; --- delay still memoizes after force ---
(define counter 0)
(define dp
  (delay (begin (set! counter (+ counter 1)) 'done)))
(force dp)
(force dp)
(force dp)
(test-eqv "delay-memoizes" 1 counter)

; --- forcing a non-promise returns it unchanged ---
(test-eqv "force-non-promise-int"  42      (force 42))
(test-equal "force-non-promise-list" '(1 2) (force '(1 2)))
(test-equal "force-non-promise-str"  "hi"  (force "hi"))

; --- delay-force in a tail position: trampolines ---
(define (loop-down n)
  (if (= n 0)
      (delay 'done)
      (delay-force (loop-down (- n 1)))))
(test-eqv "loop-down-large" 'done (force (loop-down 500)))

; --- make-promise wraps a non-promise as a forced promise ---
(define mp (make-promise 'wrapped))
(test-true  "make-promise-is-promise" (promise? mp))
(test-eqv   "force-make-promise" 'wrapped (force mp))
