; Replicated key/value cache — in CrabScheme, on the Raft engine.
;
; A KV cache is just a pure state machine declared as a replicated actor.
; Per CONSTITUTION.md Article VIII the state machine is deterministic (no
; current-time / random / I/O), so every replica that applies the same
; committed log reaches the same state.
;
; Running this file (`crabscheme run lib/consensus/kv-cache.scm`) executes the
; self-test at the bottom: a 3-node cluster elects a leader, commits a sequence
; of writes, and every replica converges to the same store.

(include "lib/consensus/raft.scm")   ; path is relative to the working directory
(include "lib/consensus/pmap.scm")   ; pure persistent map for the state machine

; ============================================================
; KV state machine (pure, O(log n))  —  op is (set k v) | (del k)
; ============================================================
; State is a pmap (a pure persistent map), so each replica keeps its own
; immutable snapshot — no mutable hashtable, Article II preserved.

(define (kv-empty) (pmap string<?))
(define (kv-ref m k) (pmap-ref m k #f))
(define (kv-apply state op)
  (case (car op)
    ((set) (pmap-set state (cadr op) (caddr op)))
    ((del) (pmap-del state (cadr op)))
    (else  state)))

; ============================================================
; The user-facing surface — DESIGN-DRAFT (needs the consensus + cluster primops)
; ============================================================
;
;   (define-replicated-actor kv-cache
;     #:initial '()
;     #:cluster '(node-a node-b node-c)
;     #:consistency 'linearizable
;     #:state-machine kv-apply)
;
;   (replicated-actor-call! kv-cache '(set "user:1" "alice"))   ; commit on a majority
;   (replicated-actor-read! kv-cache)                           ; linearizable snapshot
;
; `define-replicated-actor` lowers to a `raft-actor` (raft.scm, design-draft)
; whose #:state-machine is `kv-apply`. Until the cluster send/recv primops are
; wired it is illustrative; the engine it lowers to is real and tested below.

; ============================================================
; Self-test: prove election + replication + commit + apply (Articles III–IV)
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

; A 3-node cluster (tolerates 1 failure).
(define c0 (cluster-make '(a b c) kv-apply (kv-empty)))

; node a runs for office; b and c grant their votes -> a is leader.
(define c1 (cluster-campaign c0 'a))
(check "leader-elected"        #t (raft-leader? (cluster-get c1 'a)))
(check "follower-b-not-leader" #f (raft-leader? (cluster-get c1 'b)))
(check "follower-c-not-leader" #f (raft-leader? (cluster-get c1 'c)))

; Three writes through the log: SET user:1, SET user:2, DEL user:2.
(define c2 (cluster-propose c1 'a (list 'set "user:1" "alice")))
(define c3 (cluster-propose c2 'a (list 'set "user:2" "bob")))
(define c4 (cluster-propose c3 'a (list 'del "user:2")))
; Heartbeats carry the leader's commit index to the followers so they apply.
(define c5 (cluster-tick c4 'a))
(define c6 (cluster-tick c5 'a))

(check "leader-committed-3" 3 (raft-commit (cluster-get c6 'a)))

; Every replica's state machine converged to the same store.
(for-each
 (lambda (id)
   (let ((sm (raft-sm (cluster-get c6 id))))
     (check (string-append "replica-" (symbol->string id) "-user:1") "alice" (kv-ref sm "user:1"))
     (check (string-append "replica-" (symbol->string id) "-user:2") #f      (kv-ref sm "user:2"))))
 '(a b c))

(newline)
(if (> test-failures 0)
    (error "consensus self-test FAILED" test-failures)
    (begin (display "consensus self-test: all checks passed") (newline)))
