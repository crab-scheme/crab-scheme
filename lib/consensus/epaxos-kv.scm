; Replicated key/value cache on the leaderless EPaxos engine — in CrabScheme.
;
; Demonstrates the EPaxos payoff: commands on DIFFERENT keys don't interfere, so
; any replica commits them with no coordination; commands on the SAME key get a
; dependency and execute in one order that is identical on every replica — with
; no leader.
;
;   crabscheme run lib/consensus/epaxos-kv.scm

(include "lib/consensus/epaxos.scm")   ; path is relative to the working directory

; ============================================================
; KV state machine + interference (pure)
; ============================================================

(define (kv-del al k)
  (cond ((null? al) '())
        ((equal? (caar al) k) (kv-del (cdr al) k))
        (else (cons (car al) (kv-del (cdr al) k)))))
(define (kv-set al k v) (cons (cons k v) (kv-del al k)))
(define (kv-ref al k) (let ((p (assoc k al))) (if p (cdr p) #f)))
(define (kv-apply state op)
  (case (car op)
    ((set) (kv-set state (cadr op) (caddr op)))
    ((del) (kv-del state (cadr op)))
    (else  state)))

; Two ops interfere iff they touch the same key (cadr is the key for set/del).
(define (kv-interferes? a b) (equal? (cadr a) (cadr b)))

; commands executed on a replica, in execution order
(define (executed-commands st)
  (map (lambda (inst) (rec-command (cmds-get st inst))) (epaxos-executed st)))

; ============================================================
; The user-facing surface — DESIGN-DRAFT (needs the cluster primops)
; ============================================================
;
;   (define-mergeable-actor kv-cache         ; leaderless variant
;     #:cluster '(node-a node-b node-c)
;     #:interferes kv-interferes?
;     #:state-machine kv-apply)
;
;   (replicated-actor-call! kv-cache '(set "k" "v") #:from node-b)  ; any replica leads
;
; Lowers to an epaxos-replica actor over the engine in epaxos.scm.

; ============================================================
; Self-test (Articles III–IV)
; ============================================================

(define test-failures 0)
(define (check name expected actual)
  (if (equal? expected actual)
      (begin (display "  ok   ") (display name) (newline))
      (begin
        (set! test-failures (+ test-failures 1))
        (display "  FAIL ") (display name)
        (display "  expected=") (write expected)
        (display " got=") (write actual) (newline))))

; ---- 1. Non-interfering commands commute: committed + executed everywhere ----
(define n0 (epx-make '(a b c) kv-interferes? kv-apply '()))
(define ni1 (epx-inject n0 'a (lambda (st) (epaxos-propose st (list 'set "x" 1)))))
(define ni2 (epx-inject (car ni1) 'b (lambda (st) (epaxos-propose st (list 'set "y" 2)))))
(define nf  (epx-settle (car ni2) (append (cdr ni1) (cdr ni2))))

(for-each
 (lambda (id)
   (let ((sm (epaxos-sm (epx-get nf id))))
     (check (string-append "noninterf-" (symbol->string id) "-x") 1 (kv-ref sm "x"))
     (check (string-append "noninterf-" (symbol->string id) "-y") 2 (kv-ref sm "y"))))
 '(a b c))

; ---- 2. Concurrent interfering writes (same key) at two different leaders:
;         every replica executes them in the SAME order, no leader. ----
(define m0 (epx-make '(a b c) kv-interferes? kv-apply '()))
(define mi1 (epx-inject m0 'a (lambda (st) (epaxos-propose st (list 'set "k" 1)))))
(define mi2 (epx-inject (car mi1) 'c (lambda (st) (epaxos-propose st (list 'set "k" 2)))))
(define mf  (epx-settle (car mi2) (append (cdr mi1) (cdr mi2))))

(define order-a (executed-commands (epx-get mf 'a)))
(check "interf-both-executed" 2 (length order-a))
(for-each
 (lambda (id)
   (check (string-append "interf-order-" (symbol->string id))
          order-a (executed-commands (epx-get mf id))))
 '(a b c))

; And the converged value agrees on every replica.
(define kval (kv-ref (epaxos-sm (epx-get mf 'a)) "k"))
(for-each
 (lambda (id)
   (check (string-append "interf-value-" (symbol->string id))
          kval (kv-ref (epaxos-sm (epx-get mf id)) "k")))
 '(a b c))

(newline)
(if (> test-failures 0)
    (error "epaxos self-test FAILED" test-failures)
    (begin (display "epaxos self-test: all checks passed") (newline)))
