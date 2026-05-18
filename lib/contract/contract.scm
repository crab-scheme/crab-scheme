; R6RS++ §2 — Phase 2B.2: contracts as a library.
;
; Layered on top of the &contract condition type shipped in
; Phase 2D. Contracts are FIRST-CLASS VALUES (procedures) that
; wrap a target procedure with input/output predicate checks.
; On violation they raise a &contract condition, which existing
; R6RS guard/raise machinery catches.
;
; Surface (this iter):
;   (contract domain-pred range-pred)
;     -> a contract value c
;   (apply-contract c proc name)
;     -> proc wrapped so each call checks args against
;        domain-pred and the result against range-pred
;   (-> domain-pred range-pred)
;     -> shorthand for (contract dom rng); composable
;
; Each call to a wrapped procedure that violates the contract
; raises a &contract condition. Blame info (source/target/
; contract description/value) is filled in via
; make-contract-violation.
;
; Tail-call discipline: the wrapper is a `lambda` not a
; `case-lambda`; its tail position is the underlying procedure
; call AFTER the args check, so tail calls through a wrapper
; preserve themselves. The range check fires AFTER the
; underlying proc returns, so the wrapper itself is non-tail at
; that point -- acceptable for the call's perspective.
;
; Limitations (deferred):
;   * Multi-arg domain check expects a single domain predicate
;     applied per-arg. Future iter: per-arg domains via
;     (-> dom1 dom2 ... rng).
;   * No `provide/contract` library-export integration yet.
;     For now users wrap with (define guarded (apply-contract c
;     proc 'name)) explicitly.
;   * Higher-order contracts (procedures-as-arguments) covered
;     in Phase 2B.4.
;   * `or/c` / `and/c` / `list/c` combinators in Phase 2B.5.

(define (make-contract domain-pred range-pred)
  ; A contract is internally a record of (dom, rng); apply-contract
  ; consumes both. We encode as a tagged vector for portability.
  (vector '__contract__ domain-pred range-pred))

(define (contract? c)
  (and (vector? c)
       (>= (vector-length c) 1)
       (eq? (vector-ref c 0) '__contract__)))

(define (contract-domain c)
  (vector-ref c 1))

(define (contract-range c)
  (vector-ref c 2))

; Shorthand: (-> dom rng) builds a contract.
(define (-> domain-pred range-pred)
  (make-contract domain-pred range-pred))

; Wrap proc with the contract c. `name` is the symbolic name of
; the wrapped procedure, used in blame info on violation.
(define (apply-contract c proc name)
  (if (not (contract? c))
      (error 'apply-contract "not a contract" c))
  (if (not (procedure? proc))
      (error 'apply-contract "not a procedure" proc))
  (let ((dom (contract-domain c))
        (rng (contract-range c)))
    (lambda args
      ; Domain check: every arg must satisfy dom.
      (let loop ((rest args))
        (cond
          ((null? rest) #t)
          ((not (dom (car rest)))
           (raise (make-contract-violation
                    'caller
                    name
                    (list '-> dom rng)
                    (car rest))))
          (else (loop (cdr rest)))))
      ; Underlying call (non-tail because we have a result
      ; check after; document the trade-off in the doc above).
      (let ((result (apply proc args)))
        (if (not (rng result))
            (raise (make-contract-violation
                     'callee
                     name
                     (list '-> dom rng)
                     result)))
        result))))
