; (match SUBJECT clause ...) — Racket-style pattern matching.
; Spec: docs/research/r6rs_extensions_spec.md §1.
; Implemented as a library on top of `define-syntax-parser` (#32;
; was `syntax-rules`); no expander or VM changes beyond the
; cs-expand fixes for #111 + #112.
;
; A clause is one of:
;   (PATTERN BODY)
;   (PATTERN (when GUARD) BODY)
;
; Supported pattern forms:
;   _                       wildcard
;   identifier              binds the subject to identifier
;   'datum                  literal compared via equal?
;   (quote datum)           same
;   ()                      empty list (null?)
;   (P1 . P2)               pair pattern (car matches P1, cdr P2)
;   (P1 P2 ... Pn)          list pattern (sugar for nested pair)
;   (cons P1 P2)            explicit pair-pattern (Racket-style)
;   (list P ...)            explicit list pattern (Racket-style)
;   (? PRED)                predicate test, no binding
;   (? PRED NAME)           predicate test, binds NAME on success
;   (vector P1 ... Pn)      vector of exact length n
;
; Deferred:
;   - bare numeric / string / boolean / character literals
;     (require explicit quoting today)
;   - ellipsis patterns (P ...) inside list / vector
;   - quasi-quote patterns
;   - record patterns (depend on the record-shorthand from §8)

(define-syntax-parser match
  ((_ subj clause ...)
   (let ((v subj))
     (match-clauses v clause ...))))

(define-syntax-parser match-clauses
  #:literals (when)
  ((_ subj)
   (error 'match "no clause matched" subj))
  ((_ subj (pat (when guard) body) rest ...)
   (match-pattern subj pat
                  (if guard body (match-clauses subj rest ...))
                  (match-clauses subj rest ...)))
  ((_ subj (pat body) rest ...)
   (match-pattern subj pat
                  body
                  (match-clauses subj rest ...))))

(define-syntax-parser match-pattern
  #:literals (_ ? quote cons list vector)
  ((_ subj _ success fail) success)

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

  ((_ subj (vector p ...) success fail)
   (if (and (vector? subj)
            (= (vector-length subj) (match-pattern-vector-length p ...)))
       (match-vector-elems subj 0 (p ...) success fail)
       fail))

  ; Racket-style explicit (cons p1 p2). Kept as an alias for
  ; (p1 . p2) so existing code keeps working.
  ((_ subj (cons car-pat cdr-pat) success fail)
   (match-pattern subj (car-pat . cdr-pat) success fail))

  ; Racket-style explicit (list p ...). Walked as the equivalent
  ; nested-pair pattern via the dotted-tail form.
  ((_ subj (list) success fail)
   (match-pattern subj () success fail))
  ((_ subj (list p rest ...) success fail)
   (match-pattern subj (p . (list rest ...)) success fail))

  ; Native dotted pair pattern — now supported by cs-expand.
  ((_ subj (car-pat . cdr-pat) success fail)
   (if (pair? subj)
       (let ((car-v (car subj))
             (cdr-v (cdr subj)))
         (match-pattern car-v car-pat
                        (match-pattern cdr-v cdr-pat success fail)
                        fail))
       fail))

  ((_ subj var success fail)
   (let ((var subj)) success)))

; --- vector helpers -----------------------------------------------

(define-syntax-parser match-pattern-vector-length
  ((_) 0)
  ((_ p rest ...)
   (+ 1 (match-pattern-vector-length rest ...))))

(define-syntax-parser match-vector-elems
  ((_ subj idx () success fail) success)
  ((_ subj idx (p rest ...) success fail)
   (let ((elem (vector-ref subj idx)))
     (match-pattern elem p
                    (match-vector-elems subj (+ idx 1) (rest ...) success fail)
                    fail))))
