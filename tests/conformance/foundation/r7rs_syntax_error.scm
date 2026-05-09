(test-section "R7RS syntax-error in syntax-rules templates")

; --- syntax-rules template using syntax-error in unmatched branch ---
; Validates that the syntax-error keyword can appear in a syntax-rules
; template without breaking the macro definition. The actual error path
; can't be tested in this harness because it's a compile-time failure;
; we just verify the matching-branch path still works.

(define-syntax my-checker
  (syntax-rules ()
    ((_ x) x)
    ((_ x y) (syntax-error "expected exactly one argument" x y))))

(test-eqv "checker-1-arg-int"   42 (my-checker 42))
(test-equal "checker-1-arg-list" '(1 2 3) (my-checker '(1 2 3)))
(test-eqv "checker-1-arg-sym"   'hello (my-checker 'hello))

; --- syntax-error inside syntax-rules with literals ---
(define-syntax checker-with-literal
  (syntax-rules (good)
    ((_ good) 'ok)
    ((_ x)    (syntax-error "must be the literal good"))))

(test-eqv "checker-literal-good" 'ok (checker-with-literal good))

; --- syntax-error doesn't leak as a binding (it's a keyword) ---
; If syntax-error is properly handled as a special form, evaluating
; a quoted reference shouldn't crash.
(test-equal "syntax-error-quoted-symbol" 'syntax-error 'syntax-error)

; --- syntax-rules can branch via patterns to avoid expanding the
; syntax-error template. Note: R7RS specifies syntax-error fires
; whenever its template is expanded, so the macro's matching pattern
; must be the "good" path; the syntax-error path is the typecheck-style
; "die at expand time" branch.
(define-syntax expect-list
  (syntax-rules ()
    ((_ (x ...))   (list x ...))
    ((_ other)     (syntax-error "expected a list literal"))))

(test-equal "expect-list-empty"  '()        (expect-list ()))
(test-equal "expect-list-three"  '(1 2 3)   (expect-list (1 2 3)))
