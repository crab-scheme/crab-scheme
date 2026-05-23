; Actor-driven Raft cluster — REAL concurrency, in CrabScheme.
;
;   crabscheme run lib/consensus/raft-cluster.scm
;
; Three Raft replicas, each a `spawn-source` actor on its own OS thread,
; coordinating over real mailboxes (send / raw-receive / PID round-trip). They
; elect a leader, replicate a sequence of writes through the log, and every
; replica's state machine converges. This is the actor-driven counterpart to
; the deterministic in-memory sim in kv-cache.scm — same pure engine
; (raft.scm), now genuinely parallel, unblocked by `spawn-source` (the bridge
; that lets a Scheme procedure be an actor body despite Scheme values being
; !Send). The replica body is lib/consensus/raft-actor-body.scm.
;
; The main thread is NOT an actor, so it observes/steers the cluster through a
; process-global table the replicas publish to, busy-waiting on shared state
; (the replicas run in parallel on worker threads, so the spin doesn't block
; them).

(define body (call-with-input-file "lib/consensus/raft-actor-body.scm" get-string-all))
(make-table 'raft-meta "set")
(make-table 'raft-kv "set")

(define ids '(a b c))
(define (pid-of id) (cdr (assq id pids)))

; Spawn the replicas (each gets its id + the full id list — no PIDs as args),
; then hand every replica the (id . pid) map so it can route engine messages.
(define pids (map (lambda (id) (cons id (spawn-source body 'raft-replica id ids))) ids))
(for-each (lambda (p) (send (cdr p) (list 'config pids))) pids)

; ---- observation helpers (read what the replicas publish) ----
(define (meta id) (table-lookup 'raft-meta (symbol->string id)))
(define (role-of id) (let ((m (meta id))) (and m (car m))))
(define (commit-of id) (let ((m (meta id))) (if m (caddr m) 0)))

; Busy-wait until PRED holds; error out after a generous bound so a wedged
; cluster fails loudly instead of hanging.
(define (spin pred who)
  (let loop ((i 0))
    (cond ((pred) #t)
          ((> i 20000000) (error (string-append "actor-raft: timed out waiting for " who)))
          (else (loop (+ i 1))))))

; ---- 1. Election: a stands for office; b and c grant -> a is leader. ----
(send (pid-of 'a) (list 'campaign))
(spin (lambda () (eq? (role-of 'a) 'leader)) "leader election")

; ---- 2. Replicate three writes through the leader. ----
(send (pid-of 'a) (list 'propose (list 'set "user:1" "alice")))
(send (pid-of 'a) (list 'propose (list 'set "user:2" "bob")))
(send (pid-of 'a) (list 'propose (list 'del "user:2")))

; Leader commits once a quorum acks (it does not push the new commit index
; out by itself — followers learn it on the next heartbeat, exactly as in real
; Raft). Wait for the leader, then heartbeat until the followers catch up.
(spin (lambda () (>= (commit-of 'a) 3)) "leader commit")
(let flush ((ticks 0) (i 0))
  (cond ((and (>= (commit-of 'b) 3) (>= (commit-of 'c) 3)) #t)
        ((> ticks 1000) (error "actor-raft: followers never caught up"))
        ((> i 200000)                                   ; polled a while; heartbeat again
         (send (pid-of 'a) (list 'tick))
         (flush (+ ticks 1) 0))
        (else (flush ticks (+ i 1)))))

; ---- 3. Read user:1 on every replica and prove they agree. ----
(for-each (lambda (id) (send (pid-of id) (list 'get "user:1"))) ids)
(spin (lambda ()
        (and (table-lookup 'raft-kv "a:user:1")
             (table-lookup 'raft-kv "b:user:1")
             (table-lookup 'raft-kv "c:user:1")))
      "reads to resolve")

(define failures 0)
(for-each
 (lambda (id)
   (let ((v (table-lookup 'raft-kv (string-append (symbol->string id) ":user:1"))))
     (if (equal? v "alice")
         (begin (display "  ok   replica ") (display id) (display " user:1 = ") (display v) (newline))
         (begin (set! failures (+ failures 1))
                (display "  FAIL replica ") (display id) (display " user:1 = ") (write v) (newline)))))
 ids)

(newline)
(display "actor-driven raft: 3 replicas (real threads) elected a leader and")
(newline)
(display "replicated 3 writes over live mailboxes; commit index on a/b/c = ")
(display (list (commit-of 'a) (commit-of 'b) (commit-of 'c))) (newline)
(if (> failures 0)
    (error "actor-raft cluster: replicas DISAGREE" failures)
    (begin (display "actor-driven raft cluster: all checks passed") (newline)))
