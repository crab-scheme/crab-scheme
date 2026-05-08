(test-section "Macro hygiene: gensym-renamed binders")

; swap! is the canonical hygiene test. Without hygiene, the macro's internal
; `tmp` would capture the user's `tmp`.
(define-syntax swap!
  (syntax-rules ()
    ((_ a b)
     (let ((tmp a))
       (set! a b)
       (set! b tmp)))))

; Case 1: user has no `tmp` — straightforward
(define a1 10)
(define b1 20)
(swap! a1 b1)
(test-eqv "swap-basic-a" 20 a1)
(test-eqv "swap-basic-b" 10 b1)

; Case 2: user has a `tmp` variable — must NOT be clobbered by macro's internal tmp
(define tmp 999)
(define a2 1)
(define b2 2)
(swap! a2 b2)
(test-eqv "user-tmp-preserved" 999 tmp)
(test-eqv "swap-with-user-tmp-a" 2 a2)
(test-eqv "swap-with-user-tmp-b" 1 b2)

; Case 3: user passes their own `tmp` as one of the args — macro must work
(define other 100)
(swap! tmp other)
(test-eqv "swap-user-tmp-a"     100 tmp)
(test-eqv "swap-user-tmp-other" 999 other)
; restore
(set! tmp 999) (set! other 100)

; Lambda binder hygiene: macro introduces a lambda
(define-syntax my-incr-fn
  (syntax-rules ()
    ((_) (lambda (x) (+ x 1)))))
(define x-outer 10)
(define inc (my-incr-fn))
(test-eqv "lambda-no-capture-of-outer-x" 6 (inc 5))
(test-eqv "outer-x-untouched" 10 x-outer)

; do loop hygiene: macro generates do with internal counter
(define-syntax sum-to
  (syntax-rules ()
    ((_ n)
     (do ((i 0 (+ i 1)) (s 0 (+ s i)))
         ((= i n) s)))))
(define i 999)
(define s 999)
(test-eqv "macro-do-not-capture-user-i" 45 (sum-to 10))
(test-eqv "user-i-preserved" 999 i)
(test-eqv "user-s-preserved" 999 s)

; Macro-introduced let with multiple bindings
(define-syntax with-3-tmps
  (syntax-rules ()
    ((_ body)
     (let ((a 1) (b 2) (c 3))
       body))))
(define a 100)
(define b 200)
(test-eqv "macro-let-no-capture-a"  200 (begin (with-3-tmps 'ignored) b))
(test-eqv "macro-let-evaluates-body" 'X (with-3-tmps 'X))

; Pattern var passed to macro stays unrenamed
(define-syntax echo-name
  (syntax-rules ()
    ((_ name) (let ((local 99)) name))))
(define hello 42)
(test-eqv "pattern-var-stays-original" 42 (echo-name hello))

; or-style macro with internal `t` (would normally capture without hygiene)
(define-syntax my-or2
  (syntax-rules ()
    ((_) #f)
    ((_ e1 e2 ...) (let ((t e1)) (if t t (my-or2 e2 ...))))))
(define t 'user-t)
(test-eqv "my-or2-finds-truthy" 7 (my-or2 #f #f 7))
(test-eqv "user-t-preserved-after-or" 'user-t t)
