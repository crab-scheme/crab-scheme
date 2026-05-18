; R6RS++ §2 — contracts as a library.
;
; Layered on top of the &contract condition type shipped in
; Phase 2D. Contracts are FIRST-CLASS VALUES (records) that
; wrap a target procedure with input/output predicate checks.
; On violation they raise a &contract condition, which existing
; R6RS guard/raise machinery catches.
;
; Surface:
;   (-> dom-pred-or-contract ... rng-pred-or-contract)
;     -> a contract value
;
;     Last arg is the range predicate; all earlier args are
;     domain predicates. Single-domain form `(-> dom rng)`
;     applies dom to every arg (variadic); multi-domain form
;     `(-> dom1 dom2 ... rng)` enforces fixed arity matching
;     the domain count.
;
;     A "domain" can be either a predicate (procedure of one
;     argument returning a boolean) OR another contract. When
;     it's a contract and the corresponding arg is a procedure,
;     the arg is wrapped via apply-contract (higher-order
;     contract; see Phase 2B.4 in the plan).
;
;   (make-contract '(dom-pred-or-contract ...) rng-pred-or-contract)
;     Lower-level constructor; takes domains as a list.
;
;   (contract? v)        -> boolean predicate
;   (apply-contract c proc 'name)
;     Wraps proc with c. Each call checks args against domains
;     (or wraps procedure args with sub-contracts) and the
;     return value against the range. On violation raises
;     &contract with caller/callee blame.
;
; Blame:
;   - Caller blamed for argument violations
;   - Callee blamed for return-value violations
;   - For higher-order: the sub-contract wraps the procedure
;     arg with the wrapped proc's name as the blame label, so
;     misuse blames the right side.
;
; Tail-call note: the wrapper does the underlying call
; non-tail because there's a range check after. The underlying
; call itself sees the wrapper's tail frame as its caller, so
; tail calls FROM the wrapped proc out continue to work.

(define (make-contract domain-preds range-pred)
  ; domain-preds: list of predicates or contracts (per-arg).
  ;   Single-element list = variadic (apply to every arg).
  ;   Multi-element list = fixed-arity (per-position checks).
  ; range-pred: predicate or contract for the return value.
  (vector '__contract__ domain-preds range-pred))

(define (contract? c)
  (and (vector? c)
       (>= (vector-length c) 1)
       (eq? (vector-ref c 0) '__contract__)))

(define (contract-domains c)
  (vector-ref c 1))

(define (contract-range c)
  (vector-ref c 2))

; (-> dom ... rng): last arg is range; earlier args are domains.
; At least 2 args required (one domain + one range). Uses
; variadic-lambda form `(lambda preds ...)` rather than
; `(define (-> . preds) ...)` because the latter isn't yet
; supported by cs-expand's define-shape parser.
(define ->
  (lambda preds
    (if (< (length preds) 2)
        (error '-> "needs at least one domain + one range" preds))
    (let loop ((rest preds) (doms-acc '()))
      (if (null? (cdr rest))
          (make-contract (reverse doms-acc) (car rest))
          (loop (cdr rest) (cons (car rest) doms-acc))))))

; Internal: check / wrap one arg through a domain spec. A spec
; that's a contract gets used to wrap (if arg is a procedure);
; a spec that's a predicate gets called for a boolean check.
; Returns the (possibly wrapped) arg on success; raises on
; failure.
(define (__apply-domain spec arg name contract-desc)
  (cond
    ((contract? spec)
     (if (procedure? arg)
         (apply-contract spec arg name)
         (raise (make-contract-violation 'caller name contract-desc arg))))
    ((procedure? spec)
     (if (spec arg)
         arg
         (raise (make-contract-violation 'caller name contract-desc arg))))
    (else
     (error 'apply-contract "domain spec must be predicate or contract" spec))))

(define (__apply-range spec result name contract-desc)
  (cond
    ((contract? spec)
     (if (procedure? result)
         (apply-contract spec result name)
         (raise (make-contract-violation 'callee name contract-desc result))))
    ((procedure? spec)
     (if (spec result)
         result
         (raise (make-contract-violation 'callee name contract-desc result))))
    (else
     (error 'apply-contract "range spec must be predicate or contract" spec))))

; Wrap proc with the contract c. `name` identifies the wrapped
; procedure in blame messages.
(define (apply-contract c proc name)
  (if (not (contract? c))
      (error 'apply-contract "not a contract" c))
  (if (not (procedure? proc))
      (error 'apply-contract "not a procedure" proc))
  (let* ((doms (contract-domains c))
         (rng (contract-range c))
         (desc (list '-> doms rng)))
    (lambda args
      ; Build checked-args via an explicit loop. We avoid `map`
      ; here because at the time of writing, our `map` builtin
      ; doesn't reliably propagate exceptions raised from inside
      ; its callback (raised conditions become uncaught even
      ; when the call is wrapped in `guard`). The explicit loop
      ; lets `raise` unwind cleanly back to the user's `guard`.
      (let* ((checked-args
              (cond
                ; Single-domain variadic: apply dom to every arg.
                ((= (length doms) 1)
                 (let ((dom (car doms)))
                   (let loop ((rest args) (acc '()))
                     (if (null? rest)
                         (reverse acc)
                         (loop (cdr rest)
                               (cons (__apply-domain dom (car rest) name desc)
                                     acc))))))
                ; Multi-domain fixed-arity: arity must match
                ; len(doms), each arg checked against its position.
                (else
                 (if (not (= (length args) (length doms)))
                     (raise (make-contract-violation
                              'caller
                              name
                              desc
                              (list 'arity-mismatch
                                    'expected (length doms)
                                    'got (length args)))))
                 (let loop ((ds doms) (as args) (acc '()))
                   (if (null? ds)
                       (reverse acc)
                       (loop (cdr ds)
                             (cdr as)
                             (cons (__apply-domain (car ds) (car as) name desc)
                                   acc)))))))
             (result (apply proc checked-args)))
        (__apply-range rng result name desc)))))

; ============================================================
; Phase 2B.5 — combinators
;
; Combinators build new predicates out of existing ones. They
; return PREDICATES (one-arg procedures returning a boolean), so
; they slot into `(-> dom rng)` without any grammar changes —
; e.g. `(-> (or/c number? string?) any/c)`.
;
;   (or/c p1 p2 ...)   disjunction: succeeds if any pi accepts v
;   (and/c p1 p2 ...)  conjunction: succeeds if all pi accept v
;   (list/c p1 p2 ...) fixed-length list, per-position checks
;   any/c              accepts anything
;   none/c             accepts nothing
;
; Note: these intentionally return plain predicates rather than
; contract records. We don't need higher-order behavior for the
; combinators themselves; the user can still pass a `make-contract`
; record as any pi if they want HO semantics at that position.

(define (any/c x) #t)
(define (none/c x) #f)

(define or/c
  (lambda preds
    (lambda (v)
      (let loop ((ps preds))
        (cond
          ((null? ps) #f)
          (((car ps) v) #t)
          (else (loop (cdr ps))))))))

(define and/c
  (lambda preds
    (lambda (v)
      (let loop ((ps preds))
        (cond
          ((null? ps) #t)
          (((car ps) v) (loop (cdr ps)))
          (else #f))))))

; (list/c p1 p2 ... pn) — accepts a proper list of length n where
; element i satisfies pi.
(define list/c
  (lambda preds
    (let ((n (length preds)))
      (lambda (v)
        (and (list? v)
             (= (length v) n)
             (let loop ((ps preds) (xs v))
               (cond
                 ((null? ps) #t)
                 (((car ps) (car xs)) (loop (cdr ps) (cdr xs)))
                 (else #f))))))))

; ============================================================
; Phase 2B.6 — define/contract and provide/contract
;
; `define/contract` attaches a contract to a top-level binding in
; one step. Because the bound name IS the wrapped procedure, an
; ordinary `(export ...)` clause from any enclosing library re-
; exports the wrapped version transparently — callers receive the
; contract-protected closure, blame label included.
;
;   (define/contract name contract expr)
;     -> (define name (apply-contract contract expr 'name))
;
; `provide/contract` is the Racket-style sugar for several at once.
; It expands to a sequence of `define/contract` forms targeting an
; already-defined function: each clause `(name contract)` is
; rewritten as `(define name (apply-contract contract name 'name))`,
; which rebinds `name` to the wrapped version in place. Putting
; `provide/contract` AFTER the relevant defines (and before the
; library boundary closes) is the intended call order.

(define-syntax define/contract
  (syntax-rules ()
    ((_ name contract expr)
     (define name (apply-contract contract expr (quote name))))))

(define-syntax provide/contract
  (syntax-rules ()
    ((_ (name contract) ...)
     (begin
       (define name (apply-contract contract name (quote name)))
       ...))))
