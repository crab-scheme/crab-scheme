; Actor body for an actor-driven Raft replica — the real thing, not the sim.
;
; This file is loaded as SOURCE by raft-cluster.scm via `spawn-source`, so it
; runs inside its own per-actor runtime on its own OS thread. It pumps the SAME
; pure transitions the deterministic simulator uses (raft.scm), but now over
; real mailboxes: it `(raw-receive)`s messages and `(send)`s the engine's
; outputs to peer actors. This is exactly the networked driver sketched as a
; design-draft at the bottom of raft.scm — now wired, because `spawn-source`
; lets actor logic be Scheme (Constitution Article I).
;
; The (non-actor) cluster driver observes progress through a process-global
; table that each replica publishes to after every transition.

(include "lib/consensus/raft.scm")   ; pure engine — CWD-relative include
(include "lib/consensus/pmap.scm")   ; pure persistent map for the KV state machine

; ---- KV state machine (pure, O(log n)) ----
(define (kv-apply st op)
  (case (car op)
    ((set) (pmap-set st (cadr op) (caddr op)))
    ((del) (pmap-del st (cadr op)))
    (else  st)))
(define (kv-ref m k) (pmap-ref m k #f))

; ---- observability: publish (role term commit) so the driver can poll ----
(define (publish! st)
  (table-insert! 'raft-meta (symbol->string (raft-id st))
                 (list (raft-role st) (raft-term st) (raft-commit st))))

; ---- routing: map engine target-ids to PIDs and send (engine FROM msg) ----
; `peers` is an alist (id . pid) for the whole cluster; the engine only ever
; targets actual peers, so self never appears as a target.
(define (emit! peers from outs)
  (for-each
   (lambda (o)
     (send (cdr (assq (car o) peers)) (list 'engine from (cdr o))))
   outs))

; ---- the replica loop ----
; Protocol messages from the driver / peers:
;   (config (id . pid) ...)   learn the peer PIDs (sent first, before anything)
;   (campaign)                stand for election
;   (propose CMD)             leader appends + replicates CMD
;   (tick)                    leader heartbeat (carries the commit index)
;   (engine FROM MSG)         a raft engine message MSG from peer FROM
;   (get KEY)                 publish this replica's value for KEY into raft-kv
(define (raft-replica id ids)
  (let loop ((st (make-raft id ids kv-apply (pmap string<?)))
             (peers '()))
    (publish! st)
    (let ((msg (raw-receive)))
      (case (car msg)
        ((config)
         (loop st (cadr msg)))
        ((campaign)
         (let ((res (raft-campaign st)))
           (emit! peers id (cdr res))
           (loop (car res) peers)))
        ((propose)
         (let ((res (raft-propose st (cadr msg))))
           (emit! peers id (cdr res))
           (loop (car res) peers)))
        ((tick)
         (let ((res (raft-tick st)))
           (emit! peers id (cdr res))
           (loop (car res) peers)))
        ((engine)
         (let ((res (raft-step st (cadr msg) (caddr msg))))
           (emit! peers id (cdr res))
           (loop (car res) peers)))
        ((get)
         (table-insert! 'raft-kv
                        (string-append (symbol->string id) ":" (cadr msg))
                        (kv-ref (raft-sm st) (cadr msg)))
         (loop st peers))
        (else (loop st peers))))))
