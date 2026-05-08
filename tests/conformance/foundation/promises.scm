(test-section "Promises: delay + force")

; Basic delay/force
(test-eqv "force-immediate" 42 (force (delay 42)))

; Force on a non-promise returns it (R6RS-style)
(test-eqv "force-non-promise" 100 (force 100))

; promise? predicate
(test-true  "promise-of-delay" (promise? (delay 1)))
(test-false "promise-of-num"   (promise? 42))
(test-false "promise-of-list"  (promise? '(1 2 3)))

; Memoization: thunk runs only once
(define call-count 0)
(define p (delay (begin (set! call-count (+ call-count 1)) 99)))
(test-eqv "force-1st"      99 (force p))
(test-eqv "force-2nd-same" 99 (force p))
(test-eqv "force-3rd-same" 99 (force p))
(test-eqv "memoized-once"  1  call-count)

; Lazy evaluation: side effect happens only on force
(define count2 0)
(define lazy-incr (delay (begin (set! count2 (+ count2 1)) count2)))
(test-eqv "no-side-effect-yet" 0 count2)
(force lazy-incr)
(test-eqv "side-effect-after-force" 1 count2)

; Composing: delay can capture a closure
(define (make-counter)
  (let ((n 0))
    (delay (begin (set! n (+ n 1)) n))))
(define c1 (make-counter))
(define c2 (make-counter))
(test-eqv "first-counter-1" 1 (force c1))
(test-eqv "first-counter-memo" 1 (force c1))
(test-eqv "second-counter-1" 1 (force c2))
