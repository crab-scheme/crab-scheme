; CROSS-NODE Raft over REAL TCP SOCKETS — in CrabScheme.
;
;   crabscheme run lib/consensus/raft-net-tcp.scm
;
; Same as raft-net.scm, but the nodes are connected over actual loopback TCP
; instead of the in-memory sim transport: every RequestVote / AppendEntries is
; serialized, framed, and sent across a real socket, decoded on the far node.
; The replica body (raft-net-body.scm) is byte-for-byte the same — node-send /
; node-poll are transport-agnostic, so only the cluster wiring changes (here,
; node-listen / node-connect instead of node-link!).
;
; Because a TCP peer is registered asynchronously on the accepting side, the
; orchestrator waits on (node-peer-count …) until the mesh is fully connected
; before driving consensus.

(make-table 'raft-net-meta "set")
(make-table 'raft-net-kv "set")

(for-each node-make (list "a" "b" "c" "ctl"))

; Each replica listens on an ephemeral loopback port; ctl only dials out.
(define addr-a (node-listen "a" "127.0.0.1:0"))
(define addr-b (node-listen "b" "127.0.0.1:0"))
(define addr-c (node-listen "c" "127.0.0.1:0"))

; One full-duplex TCP connection per pair covers both directions. Replica mesh:
; b–a, c–a, c–b. Plus ctl dials every replica.
(node-connect "b" addr-a)
(node-connect "c" addr-a)
(node-connect "c" addr-b)
(node-connect "ctl" addr-a)
(node-connect "ctl" addr-b)
(node-connect "ctl" addr-c)

; Wait until every replica has its 3 peers (two replicas + ctl) registered —
; i.e. all accepting-side handshakes have completed.
(define (spin pred who)
  (let loop ((i 0))
    (cond ((pred) #t)
          ((> i 50000000) (error (string-append "raft-net-tcp: timed out waiting for " who)))
          (else (loop (+ i 1))))))
(spin (lambda () (and (= (node-peer-count "a") 3)
                      (= (node-peer-count "b") 3)
                      (= (node-peer-count "c") 3)))
      "TCP mesh to connect")

; Spawn the replica actors (real threads) — same body as the sim demo.
(define body (call-with-input-file "lib/consensus/raft-net-body.scm" get-string-all))
(for-each (lambda (id) (spawn-source body 'replica id '(a b c))) '(a b c))

; ---- observe + steer (identical to raft-net.scm) ----
(define (meta id) (table-lookup 'raft-net-meta (symbol->string id)))
(define (role-of id) (let ((m (meta id))) (and m (car m))))
(define (commit-of id) (let ((m (meta id))) (if m (caddr m) 0)))
(define (ctl! to msg) (node-send "ctl" to msg))

(ctl! "a" (list 'campaign))
(spin (lambda () (eq? (role-of 'a) 'leader)) "leader election")

(ctl! "a" (list 'propose (list 'set "user:1" "alice")))
(ctl! "a" (list 'propose (list 'set "user:2" "bob")))
(ctl! "a" (list 'propose (list 'del "user:2")))
(spin (lambda () (>= (commit-of 'a) 3)) "leader commit")

(let flush ((ticks 0) (i 0))
  (cond ((and (>= (commit-of 'b) 3) (>= (commit-of 'c) 3)) #t)
        ((> ticks 4000) (error "raft-net-tcp: followers never caught up"))
        ((> i 60000) (ctl! "a" (list 'tick)) (flush (+ ticks 1) 0))
        (else (flush ticks (+ i 1)))))

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
(display "cross-node raft over REAL TCP: 3 nodes (real threads + sockets) elected a")
(newline)
(display "leader and replicated 3 writes — every RPC over a loopback TCP connection.")
(newline)
(display "commit index on a/b/c = ")
(display (list (commit-of 'a) (commit-of 'b) (commit-of 'c))) (newline)
(if (> failures 0)
    (error "raft-net-tcp cluster: nodes DISAGREE" failures)
    (begin (display "cross-node raft over real TCP: all checks passed") (newline)))
