(test-section "cond-expand + assert error rendering")

; Basic feature lookup.
(test-equal "cond-expand-crabscheme"
  "yes"
  (cond-expand (crabscheme "yes") (else "no")))

; Else fallback.
(test-equal "cond-expand-else"
  "fallback"
  (cond-expand (no-such-feature "skipped") (else "fallback")))

; (and ...) compound.
(test-equal "cond-expand-and-true"
  "both"
  (cond-expand ((and r6rs-subset r7rs-subset) "both") (else "neither")))

(test-equal "cond-expand-and-false"
  "neither"
  (cond-expand ((and r6rs-subset no-such) "both") (else "neither")))

; (or ...) compound.
(test-equal "cond-expand-or"
  "any"
  (cond-expand ((or no-such crabscheme) "any") (else "neither")))

; (not ...).
(test-equal "cond-expand-not"
  "default"
  (cond-expand ((not crabscheme) "skipped") (else "default")))

; (library ...) — we have no library system, so always false.
(test-equal "cond-expand-library-false"
  "no library system"
  (cond-expand ((library (scheme base)) "yes") (else "no library system")))

; First matching clause wins, even if a later one would match.
(test-equal "cond-expand-first-wins"
  "first"
  (cond-expand
    (crabscheme "first")
    (crabscheme "second")
    (else "fallback")))

; Multiple-expression body in a clause runs sequentially with the last
; expression's value as the result.
(test-eqv "cond-expand-multi-body"
  42
  (cond-expand
    (crabscheme
      (define (cond-expand-helper) 42)
      (cond-expand-helper))
    (else 0)))

; assert error message includes the source form of the failed expression.
; Uses the proper R6RS accessor — condition-message — rather than poking at
; the raised condition's internal layout.
(test-true "assert-message-has-form"
  (with-exception-handler
    (lambda (c)
      (and (condition? c)
           (message-condition? c)
           (let ((msg (condition-message c)))
             (and (string? msg)
                  (string-contains msg "(= 1 2)")))))
    (lambda () (assert (= 1 2)) #f)))
