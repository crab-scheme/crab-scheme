; Replica actor body for a CROSS-NODE Raft cluster — loaded by spawn-source.
;
; Each replica is an actor on its own OS thread that owns its node's Raft state
; and communicates with the other nodes ONLY over cs-net: it `(node-poll)`s its
; node for inbound messages and `(node-send)`s the engine's outputs to the
; target nodes. No shared mailbox, no shared state — messages are serialized,
; framed, and routed through cs-distrib's Router over a cs-net transport, then
; decoded on the far side. Same pure engine (raft.scm) as the in-memory sim and
; the in-process actor cluster; only the transport changes.
;
; The (non-actor) orchestrator (raft-net.scm) creates the nodes + links and
; observes progress through a process-global table the replicas publish to.

(include "lib/consensus/raft.scm")
(include "lib/consensus/pmap.scm")

(define (kv-apply st op)
  (case (car op)
    ((set) (pmap-set st (cadr op) (caddr op)))
    ((del) (pmap-del st (cadr op)))
    (else  st)))
(define (kv-ref m k) (pmap-ref m k #f))

; publish observable state for the orchestrator to poll
(define (publish! st)
  (table-insert! 'raft-net-meta (symbol->string (raft-id st))
                 (list (raft-role st) (raft-term st) (raft-commit st))))

; ship each engine output (target-id . engine-msg) to that node over cs-net
(define (emit! from outs)
  (for-each
   (lambda (o)
     (node-send (symbol->string from) (symbol->string (car o))
                (list 'engine from (cdr o))))
   outs))

; one inbound protocol message -> (st' . outputs)
(define (dispatch st msg)
  (case (car msg)
    ((campaign) (raft-campaign st))
    ((propose)  (raft-propose st (cadr msg)))
    ((tick)     (raft-tick st))
    ((engine)   (raft-step st (cadr msg) (caddr msg)))
    ((get)
     (table-insert! 'raft-net-kv
                    (string-append (symbol->string (raft-id st)) ":" (cadr msg))
                    (kv-ref (raft-sm st) (cadr msg)))
     (cons st '()))
    (else (cons st '()))))

(define (handle-all id st msgs)
  (if (null? msgs) st
      (let ((res (dispatch st (car msgs))))
        (emit! id (cdr res))
        (handle-all id (car res) (cdr msgs)))))

; The replica loop: poll the transport; process any messages (publishing the
; new state); when idle, (yield) so this non-blocking poll loop releases its
; worker thread cooperatively instead of starving its peers.
(define (replica id ids)
  (let ((st0 (make-raft id ids kv-apply (pmap string<?))))
    (publish! st0)
    (let loop ((st st0))
      (let ((msgs (node-poll (symbol->string id))))
        (if (pair? msgs)
            (let ((st2 (handle-all id st msgs)))
              (publish! st2)
              (loop st2))
            (begin (yield) (loop st)))))))
