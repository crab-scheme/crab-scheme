(test-section "call/cc — escape continuations")

; Basic: continuation captures and returns directly.
(test-eqv "callcc-direct"
  42
  (call/cc (lambda (k) 42)))

; Escape: invoke continuation to bypass enclosing computation.
(test-eqv "callcc-escape"
  99
  (call/cc (lambda (k) (+ 1 (k 99)))))

; Outer addition is captured by k: (k 10) jumps "as if call/cc returned 10"
; back into the (+ 1 _) context, giving 11.
(test-eqv "callcc-add-around"
  11
  (+ 1 (call/cc (lambda (k) (k 10)))))

; Searching: use continuation to early-exit a fold.
(test-eqv "callcc-find-first-zero"
  0
  (call/cc
    (lambda (return)
      (for-each (lambda (x) (if (= x 0) (return x) #f))
                '(3 5 7 0 9))
      'not-found)))

; Continuation invoked with no arg yields unspecified — coerced via if.
(test-true "callcc-no-arg"
  (call/cc (lambda (k) #t)))

; call-with-current-continuation alias.
(test-eqv "callcc-long-name"
  7
  (call-with-current-continuation (lambda (k) 7)))

; Nested call/cc: inner k escapes only the inner.
(test-eqv "callcc-nested-inner"
  100
  (+ 50 (call/cc (lambda (k1)
                   (+ 10 (call/cc (lambda (k2) (k2 40))))))))

; Outer k escapes both.
(test-eqv "callcc-nested-outer"
  77
  (call/cc (lambda (k1)
             (+ 10 (call/cc (lambda (k2) (k1 77)))))))

; Continuation captured but never invoked: thunk returns naturally.
(test-eqv "callcc-uninvoked"
  5
  (let ((stash #f))
    (call/cc (lambda (k) (set! stash k) 5))))
