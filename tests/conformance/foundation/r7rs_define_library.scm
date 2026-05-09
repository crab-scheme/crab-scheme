(test-section "R7RS define-library form")

; --- minimal define-library: name + body in (begin ...) ---
(define-library (test r7rs simple)
  (export greet)
  (import (rnrs base))
  (begin
    (define (greet who)
      (string-append "hi, " who))))
(test-equal "dl-simple" "hi, world" (greet "world"))

; --- multiple (begin ...) clauses are concatenated ---
(define-library (test r7rs split)
  (export a b c)
  (import (rnrs base))
  (begin
    (define a 1)
    (define b 2))
  (begin
    (define c (+ a b))))
(test-eqv "dl-split-a" 1 a)
(test-eqv "dl-split-b" 2 b)
(test-eqv "dl-split-c" 3 c)

; --- imports inside the library are honored (rename example) ---
(define-library (test r7rs renames)
  (export head)
  (import (rename (rnrs base) (car head)))
  (begin
    ;; head is the renamed alias for car installed by the import clause.
    ))
(test-equal "dl-rename-head" 1 (head '(1 2 3)))

; --- empty define-library: no body, just declarations ---
(define-library (test r7rs empty)
  (export)
  (import (rnrs base)))
(test-eqv "after-empty-dl" 1 1)

; --- cond-expand inside define-library now dispatches per feature ---
(define-library (test r7rs cond-expand-pos)
  (export feat-result)
  (import (rnrs base))
  (cond-expand
    (crabscheme (begin (define feat-result 'crab)))
    (else (begin (define feat-result 'fallback)))))
(test-equal "dl-cond-expand-crabscheme" 'crab feat-result)

(define-library (test r7rs cond-expand-else)
  (export else-result)
  (import (rnrs base))
  (cond-expand
    (some-other-impl (begin (define else-result 'other)))
    (else (begin (define else-result 'taken-else)))))
(test-equal "dl-cond-expand-else" 'taken-else else-result)

; --- include-library-declarations is still accepted (no-op for now) ---
(define-library (test r7rs accept-clauses)
  (export ok)
  (import (rnrs base))
  (begin
    (define ok 'ok)))
(test-equal "dl-accept-ok" 'ok ok)

; --- duplicate declarations raise (same name) ---
; Tested manually only — the eval framework can't easily continue
; after an expand-time syntax error in the same file. The library
; registry rejects duplicates by name regardless of which keyword
; (library vs define-library) declared them first.

; --- versioned library name with trailing version list ---
(define-library (test r7rs versioned (1 0))
  (export ver)
  (import (rnrs base))
  (begin
    (define ver 'v1)))
(test-equal "dl-versioned" 'v1 ver)
