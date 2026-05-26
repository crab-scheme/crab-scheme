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

; parallel-runtime spec C5.3 — region values escape their
; lifetime when they cross a contract boundary. A region
; pair returned from a contracted proc may be reached
; through the contract wrapper after the underlying region
; drops, leaving a dangling handle. We reject region values
; with a clear &contract violation rather than letting them
; through to silently corrupt memory later.
;
; `(gc-allocator v)` returns 'region for region-backed
; heaps, 'rc for the default, 'leaf for non-heap values.
; The check fires only on heap values; leaf values pass
; through unchanged.
(define (__reject-region-or v blame name contract-desc)
  (if (eq? (gc-allocator v) 'region)
      (raise (make-contract-violation
              blame name
              (cons 'no-region-escape contract-desc)
              v))
      v))

; Internal: check / wrap one arg through a domain spec. A spec
; that's a contract gets used to wrap (if arg is a procedure);
; a spec that's a predicate gets called for a boolean check.
; Returns the (possibly wrapped) arg on success; raises on
; failure.
(define (__apply-domain spec arg name contract-desc)
  ; C5.3: callee→caller flow (an arg's region-ness applies
  ; symmetrically: a callee shouldn't accept a region value
  ; either, because the contract wrapper retains the
  ; reference past the call boundary).
  (__reject-region-or arg 'caller name contract-desc)
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
  ; C5.3: caller→callee flow — a returned region value would
  ; outlive its `(with-region …)` scope as soon as the
  ; caller stores it, so we refuse here.
  (__reject-region-or result 'callee name contract-desc)
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

; ---- eta-elision fast path (Phase 2B.7) ----
;
; When every domain spec and the range spec are plain predicates
; (not sub-contracts), we skip the `cond (contract? spec)` branch
; inside __apply-domain / __apply-range on every call. Detection
; happens once at apply-contract time so the per-call hot path only
; calls the predicate directly.

(define (__all-simple-preds? doms rng)
  (and (procedure? rng)
       (not (contract? rng))
       (let loop ((ds doms))
         (cond
           ((null? ds) #t)
           ((contract? (car ds)) #f)
           ((procedure? (car ds)) (loop (cdr ds)))
           (else #f)))))

; Fast wrapper — single-domain variadic, all specs are plain preds.
(define (__apply-contract-fast-variadic proc name desc dom rng)
  (lambda args
    (let* ((checked
            (let loop ((rest args) (acc '()))
              (if (null? rest)
                  (reverse acc)
                  (let ((v (car rest)))
                    (__reject-region-or v 'caller name desc)
                    (if (dom v)
                        (loop (cdr rest) (cons v acc))
                        (raise (make-contract-violation
                                 'caller name desc v))))))))
      (let ((result (apply proc checked)))
        (__reject-region-or result 'callee name desc)
        (if (rng result)
            result
            (raise (make-contract-violation 'callee name desc result)))))))

; Fast wrapper — fixed-arity, all specs are plain preds.
(define (__apply-contract-fast-fixed proc name desc doms rng n-doms)
  (lambda args
    (if (not (= (length args) n-doms))
        (raise (make-contract-violation
                 'caller name desc
                 (list 'arity-mismatch 'expected n-doms 'got (length args)))))
    (let* ((checked
            (let loop ((ds doms) (as args) (acc '()))
              (if (null? ds)
                  (reverse acc)
                  (let ((v (car as)))
                    (__reject-region-or v 'caller name desc)
                    (if ((car ds) v)
                        (loop (cdr ds) (cdr as) (cons v acc))
                        (raise (make-contract-violation
                                 'caller name desc v))))))))
      (let ((result (apply proc checked)))
        (__reject-region-or result 'callee name desc)
        (if (rng result)
            result
            (raise (make-contract-violation 'callee name desc result)))))))

; Wrap proc with the contract c. `name` identifies the wrapped
; procedure in blame messages.
;
; If the contract has a rest-pred slot (set by `->*`), dispatch
; to the variadic-tail wrapper; otherwise use the standard arrow
; semantics (single-domain variadic OR multi-domain fixed-arity).
; Within the arrow path, a monomorphic contract (all specs plain
; predicates, no sub-contracts) takes the eta-elision fast path.
(define (apply-contract c proc name)
  (if (not (contract? c))
      (error 'apply-contract "not a contract" c))
  (if (not (procedure? proc))
      (error 'apply-contract "not a procedure" proc))
  (if (contract-rest c)
      (apply-contract-rest c proc name)
      (apply-contract-arrow c proc name)))

(define (apply-contract-arrow c proc name)
  (let* ((doms (contract-domains c))
         (rng (contract-range c))
         (desc (list '-> doms rng)))
    ; Phase 2B.7: take the fast path when all specs are plain preds.
    (if (__all-simple-preds? doms rng)
        (if (= (length doms) 1)
            (__apply-contract-fast-variadic proc name desc (car doms) rng)
            (__apply-contract-fast-fixed proc name desc doms rng (length doms)))
        (lambda args
          ; Slow path: full __apply-domain / __apply-range dispatch.
          ; Needed when at least one spec is a sub-contract (higher-order).
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
                    ; Multi-domain fixed-arity.
                    (else
                     (if (not (= (length args) (length doms)))
                         (raise (make-contract-violation
                                  'caller name desc
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
            (__apply-range rng result name desc))))))

; (->* (dom1 dom2 ... domN) rest-pred rng)
;
; Mandatory-arity-N arrow with a variadic tail. The wrapped proc
; must accept AT LEAST N args; the leading N are checked against
; the per-position domain specs, every additional arg is checked
; against rest-pred, and the result is checked against rng.
;
; Used by the cs-typer contract lowering for procedure types
; whose ProcType.rest is set (Phase 4 iter 3). Racket spells it
; `(->* mandatories optionals rest range)`; we drop the optionals
; tier — Scheme has no optional-args concept at the procedure
; level. The non-tail-rest case (no rest, just mandatories) is
; what plain `(->)` already covers.
(define (->* mandatory-doms rest-pred rng)
  (cond
    ((not (list? mandatory-doms))
     (error '->* "mandatory-doms must be a list" mandatory-doms))
    ((not (or (procedure? rest-pred) (contract? rest-pred)))
     (error '->* "rest-pred must be a predicate or contract" rest-pred))
    (else
     ; Encode as a contract record with mandatory-doms as the
     ; per-position domain list, rng as range, and rest-pred
     ; stashed in a 4th slot. apply-contract is extended below to
     ; honor the rest slot.
     (vector '__contract__ mandatory-doms rng rest-pred))))

(define (contract-rest c)
  (if (>= (vector-length c) 4)
      (vector-ref c 3)
      #f))

; Extend apply-contract: if the contract has a rest-pred (4-elt
; vector), it's a variadic-tail arrow. We override the dispatch
; below to handle that shape rather than rewriting the existing
; apply-contract; the wrapper here is a thin specialization.
;
; Note: this could be folded into apply-contract by widening its
; cond. Kept separate for now so iter 3's diff is local.
(define (apply-contract-rest c proc name)
  (let ((doms (contract-domains c))
        (rng (contract-range c))
        (rest-pred (contract-rest c)))
    (let ((desc (list '->* doms rest-pred rng))
          (n-doms (length doms)))
      (lambda args
        (let ((n-args (length args)))
          (if (< n-args n-doms)
              (raise (make-contract-violation
                       'caller name desc
                       (list 'arity-mismatch
                             'expected (string->symbol ">=")
                             n-doms 'got n-args))))
          ; Check leading n-doms positions.
          (let loop ((ds doms) (as args) (acc '()))
            (if (null? ds)
                ; Then check every remaining arg against rest-pred.
                (let rloop ((rest as) (acc acc))
                  (if (null? rest)
                      (__apply-range
                        rng
                        (apply proc (reverse acc))
                        name
                        desc)
                      (rloop (cdr rest)
                             (cons (__apply-domain rest-pred (car rest) name desc)
                                   acc))))
                (loop (cdr ds)
                      (cdr as)
                      (cons (__apply-domain (car ds) (car as) name desc)
                            acc)))))))))

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

; (list-of/c pred) — accepts a proper list (of any length) where
; EVERY element satisfies pred. The variadic-element counterpart
; to list/c, used by the cs-typer contract lowering for
; `(Listof T)` types.
(define (list-of/c pred)
  (lambda (v)
    (and (list? v)
         (let loop ((xs v))
           (cond
             ((null? xs) #t)
             ((pred (car xs)) (loop (cdr xs)))
             (else #f))))))

; (vector-of/c pred) — accepts a vector (of any length) where
; EVERY element satisfies pred. Counterpart to list-of/c for
; vectors; used by the cs-typer contract lowering for
; `(Vectorof T)` types.
(define (vector-of/c pred)
  (lambda (v)
    (and (vector? v)
         (let ((n (vector-length v)))
           (let loop ((i 0))
             (cond
               ((= i n) #t)
               ((pred (vector-ref v i)) (loop (+ i 1)))
               (else #f)))))))

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

(define-syntax-parser define/contract
  ((_ name:id contract expr)
   (define name (apply-contract contract expr (quote name)))))

(define-syntax-parser provide/contract
  ((_ (name:id contract) ...)
   (begin
     (define name (apply-contract contract name (quote name)))
     ...)))
