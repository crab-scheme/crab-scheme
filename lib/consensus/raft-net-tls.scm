; CROSS-NODE Raft over mutual-TLS TCP — in CrabScheme.
;
;   crabscheme run lib/consensus/raft-net-tls.scm
;
; Identical to raft-net-tcp.scm, but each connection runs a real TLS 1.3 MUTUAL
; handshake (both nodes present + verify a certificate) before any consensus
; traffic flows — so every RequestVote / AppendEntries is encrypted and the
; peers are authenticated. Only the wiring changes: node-listen-tls /
; node-connect-tls instead of the plaintext node-listen / node-connect. The
; replica body (raft-net-body.scm) is byte-for-byte the same.
;
; The mTLS identity is cs-net's shared self-signed DEV identity (one cert as
; both identity and root on every node) — enough to exercise the real handshake
; in-process; a production cluster loads per-node certs from a CA. Node identity
; proper is still the cs-distrib Hello (NodeId) exchanged after the TLS
; handshake.

(make-table 'raft-net-meta "set")
(make-table 'raft-net-kv "set")

(for-each node-make (list "a" "b" "c" "ctl"))

(define addr-a (node-listen-tls "a" "127.0.0.1:0"))
(define addr-b (node-listen-tls "b" "127.0.0.1:0"))
(define addr-c (node-listen-tls "c" "127.0.0.1:0"))

(node-connect-tls "b" addr-a)
(node-connect-tls "c" addr-a)
(node-connect-tls "c" addr-b)
(node-connect-tls "ctl" addr-a)
(node-connect-tls "ctl" addr-b)
(node-connect-tls "ctl" addr-c)

(define (spin pred who)
  (let loop ((i 0))
    (cond ((pred) #t)
          ((> i 50000000) (error (string-append "raft-net-tls: timed out waiting for " who)))
          (else (loop (+ i 1))))))
(spin (lambda () (and (= (node-peer-count "a") 3)
                      (= (node-peer-count "b") 3)
                      (= (node-peer-count "c") 3)))
      "mTLS mesh to connect")

(define body (call-with-input-file "lib/consensus/raft-net-body.scm" get-string-all))
(for-each (lambda (id) (spawn-source body 'replica id '(a b c))) '(a b c))

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
        ((> ticks 4000) (error "raft-net-tls: followers never caught up"))
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
(display "cross-node raft over mUTUAL TLS: 3 nodes (real threads + encrypted sockets)")
(newline)
(display "elected a leader and replicated 3 writes — every RPC over an mTLS connection.")
(newline)
(display "commit index on a/b/c = ")
(display (list (commit-of 'a) (commit-of 'b) (commit-of 'c))) (newline)
(if (> failures 0)
    (error "raft-net-tls cluster: nodes DISAGREE" failures)
    (begin (display "cross-node raft over mutual TLS: all checks passed") (newline)))
