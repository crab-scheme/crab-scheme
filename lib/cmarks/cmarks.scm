; R6RS++ §6 Phase 3A — continuation marks (naive impl).
;
; Continuation marks let user code attach key/value pairs to the
; dynamic context. They're the substrate for stack annotations,
; profilers, debuggers, tracing libraries, and dynamic context
; propagation — Racket's most-underrated feature.
;
; THIS IS THE NAIVE IMPL. It uses an internal parameter to model
; the mark chain. Differences from Racket semantics:
;
;   1. Marks are NOT tail-safe: each `with-continuation-mark` in
;      a tail-recursive loop grows the chain rather than replacing
;      the current frame's mark. A tail-call-aware VM-level
;      implementation is the next iteration of this work.
;
;   2. Marks are dynamic-scope only. There's no notion of a
;      "captured" mark set you can pass around independent of
;      the current dynamic context (no continuation-mark-set
;      first-class value yet).
;
; The surface API is fully Racket-compatible at the call shape
; level; only the timing/sharing differs. User code written
; against this layer migrates unchanged when the tail-safe impl
; lands.
;
; Surface:
;   (with-continuation-mark key val body ...)
;     Evaluates body... with the dynamic chain extended by
;     (key . val). Returns the body's value.
;
;   (current-continuation-marks)
;     Returns the full alist of (key . val) pairs, innermost-first.
;
;   (current-continuation-marks key)
;     Returns the list of all values for `key` along the current
;     chain, innermost-first.

(define *cmarks* (make-parameter '()))

(define-syntax with-continuation-mark
  (syntax-rules ()
    ((_ key val body ...)
     (parameterize ((*cmarks* (cons (cons key val) (*cmarks*))))
       body ...))))

; Two-arity: zero args returns the full alist; one arg filters by
; key. Variadic-lambda form because cs-expand doesn't yet support
; dotted-pair `define (name . args)` heads.
(define current-continuation-marks
  (lambda args
    (let ((chain (*cmarks*)))
      (cond
        ((null? args) chain)
        ((null? (cdr args))
         (let loop ((rest chain) (acc '()))
           (cond
             ((null? rest) (reverse acc))
             ((equal? (car (car rest)) (car args))
              (loop (cdr rest) (cons (cdr (car rest)) acc)))
             (else (loop (cdr rest) acc)))))
        (else
         (error 'current-continuation-marks
                "expected 0 or 1 args, got" (length args)))))))
