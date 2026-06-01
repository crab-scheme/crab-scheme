;;; (crab spec) — Ginkgo-style BDD test runner.
;;;
;;; A bundled Scheme library (global at startup). Group specs with
;;; `describe`/`context`, declare them with `it`, share setup/teardown
;;; with `before-each`/`after-each`, then `(run-specs)`. Supports focused
;;; specs (`fdescribe`/`fit`/`fcontext`), pending specs (`xdescribe`/`xit`
;;; and `(pending)`), and table-driven specs (`describe-table`/`entry`).
;;; Pairs with `(crab expect)` matchers — a raised error fails a spec.
;;;
;;; Node layout (vectors):
;;;   group = #('spec:group name befores afters children focused? pending?)
;;;   spec  = #('spec:spec  name thunk   focused? pending?)

(define (spec:new-group name focused pending)
  (vector 'spec:group name '() '() '() focused pending))

(define spec:root (spec:new-group "" #f #f))
(define spec:current spec:root)

(define (spec:reset!)
  (set! spec:root (spec:new-group "" #f #f))
  (set! spec:current spec:root))

(define (spec:add-child! g child)
  (vector-set! g 4 (append (vector-ref g 4) (list child))))

(define (spec:enter-group name focused pending body)
  (let ((g (spec:new-group name focused pending))
        (parent spec:current))
    (spec:add-child! parent g)
    (set! spec:current g)
    (body)
    (set! spec:current parent)))

(define (spec:add-spec name focused pending thunk)
  (spec:add-child! spec:current (vector 'spec:spec name thunk focused pending)))

(define (spec:add-before! thunk)
  (vector-set! spec:current 2 (append (vector-ref spec:current 2) (list thunk))))
(define (spec:add-after! thunk)
  (vector-set! spec:current 3 (append (vector-ref spec:current 3) (list thunk))))

;; ---- declaration macros ----

(define-syntax describe
  (syntax-rules () ((_ name body ...) (spec:enter-group name #f #f (lambda () body ...)))))
(define-syntax context
  (syntax-rules () ((_ name body ...) (spec:enter-group name #f #f (lambda () body ...)))))
(define-syntax fdescribe
  (syntax-rules () ((_ name body ...) (spec:enter-group name #t #f (lambda () body ...)))))
(define-syntax fcontext
  (syntax-rules () ((_ name body ...) (spec:enter-group name #t #f (lambda () body ...)))))
(define-syntax xdescribe
  (syntax-rules () ((_ name body ...) (spec:enter-group name #f #t (lambda () body ...)))))
(define-syntax xcontext
  (syntax-rules () ((_ name body ...) (spec:enter-group name #f #t (lambda () body ...)))))

(define-syntax it
  (syntax-rules () ((_ name body ...) (spec:add-spec name #f #f (lambda () body ...)))))
(define-syntax fit
  (syntax-rules () ((_ name body ...) (spec:add-spec name #t #f (lambda () body ...)))))
(define-syntax xit
  (syntax-rules () ((_ name body ...) (spec:add-spec name #f #t (lambda () body ...)))))

(define-syntax before-each
  (syntax-rules () ((_ body ...) (spec:add-before! (lambda () body ...)))))
(define-syntax after-each
  (syntax-rules () ((_ body ...) (spec:add-after! (lambda () body ...)))))

;; ---- table-driven ----

(define (entry desc . args) (cons desc args))

(define-syntax describe-table
  (syntax-rules ()
    ((_ name proc e ...)
     (spec:enter-group name #f #f
                       (lambda ()
                         (for-each
                          (lambda (en)
                            (spec:add-spec (car en) #f #f (lambda () (apply proc (cdr en)))))
                          (list e ...)))))))

;; ---- pending signal ----

(define spec:pending-signal (list 'spec-pending))
(define (pending) (raise spec:pending-signal))

;; ---- runner ----

;; Run setup → spec thunk → teardown. Returns 'pass / 'fail / 'pending.
;; Teardown always runs; `(pending)` inside the spec yields 'pending.
(define (spec:run-one befores thunk afters)
  (let ((status (guard (e ((eq? e spec:pending-signal) 'pending)
                          (#t 'fail))
                  (for-each (lambda (b) (b)) befores)
                  (thunk)
                  'pass)))
    (guard (e (#t #f)) (for-each (lambda (a) (a)) afters))
    status))

(define (spec:focus-exists? node)
  (if (eq? (vector-ref node 0) 'spec:spec)
      (vector-ref node 3)
      (or (vector-ref node 5)
          (let any ((cs (vector-ref node 4)))
            (cond ((null? cs) #f)
                  ((spec:focus-exists? (car cs)) #t)
                  (else (any (cdr cs))))))))

;; (run-specs) — execute every registered spec, printing a line each, and
;; return (list passed failed pending skipped). Does not clear the tree;
;; call (spec:reset!) to start over.
(define (run-specs)
  (let ((pass 0) (fail 0) (pend-count 0) (skip 0)
        (focus-mode (spec:focus-exists? spec:root)))
    (let walk ((node spec:root) (befores '()) (afters '()) (focus #f) (pend #f))
      (if (eq? (vector-ref node 0) 'spec:spec)
          (let ((name (vector-ref node 1))
                (sfocus (or focus (vector-ref node 3)))
                (spend (or pend (vector-ref node 4))))
            (cond
              (spend
               (set! pend-count (+ pend-count 1))
               (display "  pend ") (display name) (newline))
              ((and focus-mode (not sfocus))
               (set! skip (+ skip 1)))
              (else
               (let ((status (spec:run-one befores (vector-ref node 2) afters)))
                 (cond
                   ((eq? status 'pass)
                    (set! pass (+ pass 1)) (display "  ok   ") (display name) (newline))
                   ((eq? status 'pending)
                    (set! pend-count (+ pend-count 1)) (display "  pend ") (display name) (newline))
                   (else
                    (set! fail (+ fail 1)) (display "  FAIL ") (display name) (newline)))))))
          ;; group: extend the before/after chains and recurse
          (let ((gb (append befores (vector-ref node 2)))
                (ga (append (vector-ref node 3) afters))
                (gf (or focus (vector-ref node 5)))
                (gp (or pend (vector-ref node 6))))
            (for-each (lambda (c) (walk c gb ga gf gp)) (vector-ref node 4)))))
    (display "specs: ") (display pass) (display " passed, ")
    (display fail) (display " failed, ")
    (display pend-count) (display " pending, ")
    (display skip) (display " skipped") (newline)
    (list pass fail pend-count skip)))
