; CROSS-NODE Raft cluster over cs-net — in CrabScheme.
;
;   crabscheme run lib/consensus/raft-net.scm
;
; Three Raft replicas, each a spawn-source actor on its own OS thread that owns
; its node's state and talks to the others ONLY over cs-net: every RequestVote /
; AppendEntries crosses as serialized data, framed with a DistPid and routed by
; cs-distrib's Router over a cs-net transport, then decoded on the far node.
; This is the cross-NODE counterpart to raft-cluster.scm (which used in-process
; mailboxes): same pure engine (raft.scm), now over the cluster transport.
;
; (The transport here is the deterministic in-memory `sim` transport — the same
; `Transport` trait the tcp/quic transports implement, so the routing /
; serialization path is identical to a real socket hop.)
;
; The orchestrator below is an ordinary (non-actor) main thread. It is itself a
; node, "ctl", linked to every replica, and it STEERS the cluster by sending
; control messages over the transport, observing progress through a
; process-global table the replicas publish to.

(make-table 'raft-net-meta "set")
(make-table 'raft-net-kv "set")

; Nodes: three replicas in a full mesh, plus a control node wired to each.
(for-each node-make (list "a" "b" "c" "ctl"))
(node-link! "a" "b") (node-link! "a" "c") (node-link! "b" "c")
(node-link! "ctl" "a") (node-link! "ctl" "b") (node-link! "ctl" "c")

; Spawn the replica actors (real threads). Each owns node a/b/c and its state.
; (Nodes + links already exist, so the replicas can route from their first
; message; control messages wait in a node's inbox until its actor polls.)
(define body (call-with-input-file "lib/consensus/raft-net-body.scm" get-string-all))
(for-each (lambda (id) (spawn-source body 'replica id '(a b c))) '(a b c))

; ---- observe + steer ----
(define (meta id) (table-lookup 'raft-net-meta (symbol->string id)))
(define (role-of id) (let ((m (meta id))) (and m (car m))))
(define (commit-of id) (let ((m (meta id))) (if m (caddr m) 0)))
(define (ctl! to msg) (node-send "ctl" to msg))      ; control message over cs-net

(define (spin pred who)
  (let loop ((i 0))
    (cond ((pred) #t)
          ((> i 50000000) (error (string-append "raft-net: timed out waiting for " who)))
          (else (loop (+ i 1))))))

; 1. Election: tell a to stand; b and c grant over the wire -> a is leader.
(ctl! "a" (list 'campaign))
(spin (lambda () (eq? (role-of 'a) 'leader)) "leader election")

; 2. Replicate three writes through the leader (AppendEntries cross the wire).
(ctl! "a" (list 'propose (list 'set "user:1" "alice")))
(ctl! "a" (list 'propose (list 'set "user:2" "bob")))
(ctl! "a" (list 'propose (list 'del "user:2")))
(spin (lambda () (>= (commit-of 'a) 3)) "leader commit")

; Followers learn the commit index on the next heartbeat — tick a until they
; catch up (real Raft; same as the in-memory driver).
(let flush ((ticks 0) (i 0))
  (cond ((and (>= (commit-of 'b) 3) (>= (commit-of 'c) 3)) #t)
        ((> ticks 4000) (error "raft-net: followers never caught up"))
        ((> i 60000) (ctl! "a" (list 'tick)) (flush (+ ticks 1) 0))
        (else (flush ticks (+ i 1)))))

; 3. Ask every replica for its value of user:1 and prove they agree.
(for-each (lambda (id) (ctl! (symbol->string id) (list 'get "user:1"))) '(a b c))
(spin (lambda ()
        (and (table-lookup 'raft-net-kv "a:user:1")
             (table-lookup 'raft-net-kv "b:user:1")
             (table-lookup 'raft-net-kv "c:user:1")))
      "reads to resolve")

(define failures 0)
(for-each
 (lambda (id)
   (let ((v (table-lookup 'raft-net-kv (string-append (symbol->string id) ":user:1"))))
     (if (equal? v "alice")
         (begin (display "  ok   node ") (display id) (display " user:1 = ") (display v) (newline))
         (begin (set! failures (+ failures 1))
                (display "  FAIL node ") (display id) (display " user:1 = ") (write v) (newline)))))
 '(a b c))

(newline)
(display "cross-node raft over cs-net: 3 nodes (real threads) elected a leader and")
(newline)
(display "replicated 3 writes — every Raft RPC serialized + routed over the transport.")
(newline)
(display "commit index on a/b/c = ")
(display (list (commit-of 'a) (commit-of 'b) (commit-of 'c))) (newline)
(if (> failures 0)
    (error "raft-net cluster: nodes DISAGREE" failures)
    (begin (display "cross-node raft over cs-net: all checks passed") (newline)))
