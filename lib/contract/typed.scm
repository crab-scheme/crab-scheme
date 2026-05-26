; R6RS++ §12 Phase 4 iter 4 — typed defines via contract lowering.
;
; Bridges cs-typer's type annotation syntax (Fixnum, Flonum, (->
; T1 T2), (Listof T), (U T1 T2)) to the contracts library. Users
; can write:
;
;   (define/typed sum (-> Fixnum Fixnum Fixnum)
;     (lambda (a b) (+ a b)))
;
; and the binding `sum` is wrapped with a contract that mirrors
; the type. Calls violating the type fire `&contract` exactly
; like a hand-written `define/contract` would.
;
; This is a SCHEME port of the Rust translator at
; crates/cs-typer/src/contract_lowering.rs. Two implementations
; of the same mapping serve different consumers:
;   - Rust: tools that want lowering as source text (the typer's
;     own pipeline integration, IDE tooling)
;   - Scheme: user-facing API that doesn't cross the Rust→Scheme
;     tier on every call
;
; Type-annotation grammar accepted by __type->contract:
;
;   Fixnum | Flonum | Boolean | Character | Symbol | Pair
;   Vector | String | ByteVector | Procedure | Null
;     -> the corresponding predicate
;
;   Any                        -> any/c
;   Never                      -> none/c
;   (U T1 T2 ...)              -> (or/c c1 c2 ...)
;   (Listof T)                 -> (list-of/c c)
;   (Vectorof T)               -> (vector-of/c c)
;   (-> T1 T2 ... Tn)          -> (-> c1 c2 ... cn)
;   (->* (T...) Tr Trng)       -> (->* (c...) cr crng)  [variadic tail]
;
; Bare type variables (any symbol not in the above set) lower to
; `any/c` — matches the polymorphism-erasure rule.

(define (__atomic-type->predicate ann)
  (cond
    ((eq? ann 'Fixnum)     integer?)
    ((eq? ann 'Flonum)     real?)
    ((eq? ann 'Boolean)    boolean?)
    ((eq? ann 'Character)  char?)
    ((eq? ann 'Symbol)     symbol?)
    ((eq? ann 'Pair)       pair?)
    ((eq? ann 'Vector)     vector?)
    ((eq? ann 'String)     string?)
    ((eq? ann 'ByteVector) bytevector?)
    ((eq? ann 'Procedure)  procedure?)
    ((eq? ann 'Null)       null?)
    ((eq? ann 'Any)        any/c)
    ((eq? ann 'Never)      none/c)
    (else #f)))

(define (__type->contract ann)
  (cond
    ; Atomic / variable case.
    ((symbol? ann)
     (or (__atomic-type->predicate ann)
         ; Unknown bare symbol = type variable, lower to any/c.
         any/c))
    ((pair? ann)
     (let ((head (car ann))
           (tail (cdr ann)))
       (cond
         ; (U T1 T2 ...) → (or/c c1 c2 ...)
         ((eq? head 'U)
          (apply or/c (map __type->contract tail)))
         ; (Listof T) → (list-of/c c)
         ((eq? head 'Listof)
          (if (and (pair? tail) (null? (cdr tail)))
              (list-of/c (__type->contract (car tail)))
              (error '__type->contract "(Listof T) needs one arg" ann)))
         ; (Vectorof T) → (vector-of/c c)
         ((eq? head 'Vectorof)
          (if (and (pair? tail) (null? (cdr tail)))
              (vector-of/c (__type->contract (car tail)))
              (error '__type->contract "(Vectorof T) needs one arg" ann)))
         ; (-> T1 ... Tn) → (-> c1 ... cn). Last positional is rng.
         ((eq? head '->)
          (if (< (length tail) 2)
              (error '__type->contract "(-> ...) needs at least 1 dom + rng" ann))
          (apply -> (map __type->contract tail)))
         ; (->* (T...) Tr Trng) → (->* (c...) cr crng)
         ((eq? head '->*)
          (if (not (= (length tail) 3))
              (error '__type->contract "(->* doms rest rng) needs 3 args" ann))
          (let ((doms (car tail))
                (rest (cadr tail))
                (rng (caddr tail)))
            (if (not (list? doms))
                (error '__type->contract "->* doms must be a list" doms))
            (->* (map __type->contract doms)
                 (__type->contract rest)
                 (__type->contract rng))))
         (else
          (error '__type->contract "unknown type form" ann)))))
    (else
     (error '__type->contract "type annotation must be symbol or list" ann))))

; (define/typed name type-ann expr)
;   ==> (define name (apply-contract (__type->contract 'type-ann)
;                                    expr
;                                    'name))
(define-syntax-parser define/typed
  ((_ name type-ann expr)
   (define name
     (apply-contract (__type->contract (quote type-ann)) expr (quote name)))))
