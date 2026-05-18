; (match SUBJECT clause ...) — Racket-style pattern matching.
; First-priority deliverable from docs/research/r6rs_extensions_spec.md
; §1. Implemented as a library on top of `syntax-rules`; no
; expander or VM changes.
;
; A clause is one of:
;   (PATTERN BODY)
;   (PATTERN (when GUARD) BODY)
;
; Supported pattern forms (v1):
;   _                       wildcard, conventional unused binding
;   identifier              binds the subject to identifier
;   'datum                  literal compared via equal?
;   (quote datum)           same
;   ()                      empty list (null?)
;   (cons P1 P2)            pair pattern (car matches P1, cdr P2)
;   (list)                  empty list (same as ())
;   (list P)                1-element list
;   (list P P)              2-element list
;   ... up to (list P P P P P P P P) (8 elements)
;   (? PRED)                predicate test, no binding
;   (? PRED NAME)           predicate test, binds NAME on success
;   (vector P1 ... Pn)      vector of exact length n
;
; v1 deviations from Racket and tracked follow-ups:
;
;   - Bare numeric / string / boolean / character literals
;     require explicit quoting today. cs-expand's syntax-rules
;     can't dispatch on atom shape; fix needs syntax-case.
;   - Dotted-pair patterns `(a . b)` and bare list patterns
;     `(a b c)` aren't supported because cs-expand's syntax-rules
;     doesn't accept dotted-pair patterns in macro templates.
;     Use `(cons a b)` and `(list a b c)` instead. Spec
;     follow-up: extend cs-expand syntax-rules to handle dotted
;     pair patterns, then add the bare forms back.
;   - Ellipsis patterns `(p ...)` and quasi-quote patterns
;     deferred to a follow-up iter (need syntax-case).

(define-syntax match
  (syntax-rules ()
    ((_ subj clause ...)
     (let ((v subj))
       (match-clauses v clause ...)))))

(define-syntax match-clauses
  (syntax-rules (when)
    ((_ subj)
     (error 'match "no clause matched" subj))
    ((_ subj (pat (when guard) body) rest ...)
     (match-pattern subj pat
                    (if guard body (match-clauses subj rest ...))
                    (match-clauses subj rest ...)))
    ((_ subj (pat body) rest ...)
     (match-pattern subj pat
                    body
                    (match-clauses subj rest ...)))))

; The pair / list / vector patterns are spelled with explicit
; head keywords (cons / list / vector) so the syntax-rules
; engine can dispatch on shape without needing dotted-pair
; pattern support.

(define-syntax match-pattern
  (syntax-rules (? quote cons list vector)
    ((_ subj 'lit success fail)
     (if (equal? subj 'lit) success fail))

    ((_ subj (quote lit) success fail)
     (if (equal? subj 'lit) success fail))

    ((_ subj () success fail)
     (if (null? subj) success fail))

    ((_ subj (? pred name) success fail)
     (if (pred subj)
         (let ((name subj)) success)
         fail))

    ((_ subj (? pred) success fail)
     (if (pred subj) success fail))

    ((_ subj (cons car-pat cdr-pat) success fail)
     (if (pair? subj)
         (let ((car-v (car subj))
               (cdr-v (cdr subj)))
           (match-pattern car-v car-pat
                          (match-pattern cdr-v cdr-pat success fail)
                          fail))
         fail))

    ((_ subj (list) success fail)
     (if (null? subj) success fail))

    ((_ subj (list p1) success fail)
     (match-pattern subj (cons p1 ()) success fail))

    ((_ subj (list p1 p2) success fail)
     (match-pattern subj (cons p1 (cons p2 ())) success fail))

    ((_ subj (list p1 p2 p3) success fail)
     (match-pattern subj (cons p1 (cons p2 (cons p3 ()))) success fail))

    ((_ subj (list p1 p2 p3 p4) success fail)
     (match-pattern subj (cons p1 (cons p2 (cons p3 (cons p4 ())))) success fail))

    ((_ subj (list p1 p2 p3 p4 p5) success fail)
     (match-pattern subj
                    (cons p1 (cons p2 (cons p3 (cons p4 (cons p5 ())))))
                    success fail))

    ((_ subj (list p1 p2 p3 p4 p5 p6) success fail)
     (match-pattern subj
                    (cons p1 (cons p2 (cons p3 (cons p4 (cons p5 (cons p6 ()))))))
                    success fail))

    ((_ subj (list p1 p2 p3 p4 p5 p6 p7) success fail)
     (match-pattern subj
                    (cons p1 (cons p2 (cons p3 (cons p4 (cons p5 (cons p6 (cons p7 ())))))))
                    success fail))

    ((_ subj (list p1 p2 p3 p4 p5 p6 p7 p8) success fail)
     (match-pattern subj
                    (cons p1 (cons p2 (cons p3 (cons p4 (cons p5 (cons p6 (cons p7 (cons p8 ()))))))))
                    success fail))

    ((_ subj (vector p ...) success fail)
     (if (and (vector? subj)
              (= (vector-length subj) (match-pattern-vector-length p ...)))
         (match-vector-elems subj 0 (p ...) success fail)
         fail))

    ((_ subj var success fail)
     (let ((var subj)) success))))

; --- vector helpers -----------------------------------------------

(define-syntax match-pattern-vector-length
  (syntax-rules ()
    ((_) 0)
    ((_ p rest ...)
     (+ 1 (match-pattern-vector-length rest ...)))))

(define-syntax match-vector-elems
  (syntax-rules ()
    ((_ subj idx () success fail) success)
    ((_ subj idx (p rest ...) success fail)
     (let ((elem (vector-ref subj idx)))
       (match-pattern elem p
                      (match-vector-elems subj (+ idx 1) (rest ...) success fail)
                      fail)))))
