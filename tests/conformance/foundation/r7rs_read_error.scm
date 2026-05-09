(test-section "R7RS read-error? on malformed reader input")

; --- valid read does not produce a read-error (and does not throw) ---
(define p1 (open-input-string "(+ 1 2)"))
(test-equal "read-valid" '(+ 1 2) (read p1))

; --- malformed input: unterminated list "(1 2 3" — read should signal a
; read-error condition that with-exception-handler can catch. ---
(define err1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda ()
          (read (open-input-string "(1 2 3")))))))
(test-true  "read-err-1-is-cond"      (error-object? err1))
(test-true  "read-err-1-is-read-err"  (read-error? err1))
(test-false "read-err-1-not-file-err" (file-error? err1))

; --- another malformed input: stray close paren ")foo" ---
(define err2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda ()
          (read (open-input-string ")")))))))
(test-true "read-err-2-is-read-err"   (read-error? err2))

; --- generic error from (error ...) is NOT a read-error ---
(define err3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (error "boom"))))))
(test-true  "generic-err-is-error"    (error-object? err3))
(test-false "generic-not-read-err"    (read-error? err3))
(test-false "generic-not-file-err"    (file-error? err3))

; --- file-error and read-error are mutually exclusive on these paths ---
(define err-file
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (open-input-file "/no/such/path/zzz/abc.txt"))))))
(test-true  "file-err-is-file-err"    (file-error? err-file))
(test-false "file-err-is-not-read"    (read-error? err-file))

; --- error-object-message survives on read-error ---
(test-true "read-err-1-has-msg-string" (string? (error-object-message err1)))
