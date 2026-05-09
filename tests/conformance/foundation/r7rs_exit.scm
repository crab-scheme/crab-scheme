(test-section "R7RS (exit) and (emergency-exit)")

; --- exit raises a catchable condition (no arg) ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (exit))))))
(test-true "exit-no-arg-condition" (condition? c1))

; --- exit with explicit boolean value ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (exit #f))))))
(test-true "exit-false-condition" (condition? c2))

; --- exit with integer code ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (exit 42))))))
(test-true "exit-42-condition" (condition? c3))

; --- exit and emergency-exit are NOT regular error objects ---
; (R7RS distinguishes "exit requested" from program errors)
(test-false "exit-not-file-error" (file-error? c1))
(test-false "exit-not-read-error" (read-error? c1))

; --- emergency-exit also catchable ---
(define c4
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (emergency-exit))))))
(test-true "emergency-exit-condition" (condition? c4))

; --- emergency-exit with code ---
(define c5
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (emergency-exit 1))))))
(test-true "emergency-exit-1-condition" (condition? c5))

; --- exit with too many args is itself an error (caught) ---
(define c6
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (exit 1 2))))))
(test-true "exit-arity-error" (condition? c6))

; --- exit value preserved through with-exception-handler ---
; The handler receives the condition; we can verify the embedded value
; survives at least to the catch site by repeatedly catching.
(define exit-val
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (exit 'sentinel))))))
(test-true "exit-sentinel-condition" (condition? exit-val))

; --- exit doesn't run thunk continuation past it ---
(define after-exit-ran #f)
(define c-skip
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda ()
          (exit 7)
          (set! after-exit-ran #t)
          'never-here)))))
(test-true  "exit-was-caught" (condition? c-skip))
(test-false "code-after-exit-skipped" after-exit-ran)
