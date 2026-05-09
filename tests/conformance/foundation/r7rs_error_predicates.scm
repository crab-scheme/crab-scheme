(test-section "R7RS error predicates: file-error?, read-error?")

; --- predicates exist and accept any value ---
(test-false "file-error?-on-int"    (file-error? 42))
(test-false "file-error?-on-string" (file-error? "boom"))
(test-false "file-error?-on-list"   (file-error? '(1 2 3)))
(test-false "file-error?-on-bool"   (file-error? #t))
(test-false "file-error?-on-null"   (file-error? '()))

(test-false "read-error?-on-int"    (read-error? 42))
(test-false "read-error?-on-string" (read-error? "boom"))
(test-false "read-error?-on-list"   (read-error? '(1 2 3)))
(test-false "read-error?-on-bool"   (read-error? #f))

; --- generic error condition: neither file nor read ---
(define generic-cond
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (error "boom" 1 2))))))
(test-true  "generic-is-error-object" (error-object? generic-cond))
(test-false "generic-not-file-error"  (file-error? generic-cond))
(test-false "generic-not-read-error"  (read-error? generic-cond))
(test-equal "generic-message" "boom" (error-object-message generic-cond))
(test-equal "generic-irritants" '(1 2) (error-object-irritants generic-cond))

; --- file-error tagged condition (from open-input-file on missing path) ---
(define file-cond
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k c))
        (lambda () (open-input-file "/this/path/cannot/possibly/exist/abc123/xyz.txt"))))))
(test-true  "file-cond-is-error-object" (error-object? file-cond))
(test-true  "file-cond-is-file-error"   (file-error? file-cond))
(test-false "file-cond-not-read-error"  (read-error? file-cond))

; --- error-object-message + irritants on file error ---
(test-true "file-cond-has-message"
  (string? (error-object-message file-cond)))

; --- predicates also do not crash on unspecified-like values ---
(test-false "file-error?-on-procedure" (file-error? car))
(test-false "read-error?-on-procedure" (read-error? cdr))
