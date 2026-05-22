; Latency model — where EPaxos's low-conflict advantage actually shows.
;
;   crabscheme run lib/consensus/latency-sim.scm
;
; The throughput bench (bench.scm) runs in a zero-latency compute sim, so it
; can't show EPaxos's win — that win is about NETWORK ROUND TRIPS, not compute.
; Here we MEASURE the number of sequential message rounds to commit a write
; (BFS depth over the message-causality graph of the real engines), then express
; commit latency as rounds x L (one-way network delay). This is the metric that
; governs real-world commit latency.
;
; Honest modeling note: Raft has no "forward to leader" / "notify origin"
; messages in our engine, so for a write that originates at a NON-leader node we
; MEASURE the leader's round-trip commit (2 rounds) and ADD the two hops Raft
; genuinely requires off-engine — forward (origin->leader) and notify
; (leader->origin) — clearly labelled below. EPaxos needs neither: any replica
; leads the command it receives.

(include "lib/consensus/raft.scm")
(include "lib/consensus/epaxos.scm")

(define (kv-apply state op) state)               ; commit-latency test ignores SM effect
(define (kv-interferes? a b) (equal? (cadr a) (cadr b)))

; ---- BFS over message waves: rounds until `committed?` holds ----
; A "round" is one one-way message hop on the critical path. The injected
; propose's outputs are wave 1; replies to those are wave 2; etc.
(define (inject cluster id action get set)
  (let* ((res (action (get cluster id)))
         (c2 (set cluster id (car res)))
         (wave (map (lambda (o) (list id (car o) (cdr o))) (cdr res))))
    (cons c2 wave)))

(define (bfs cluster wave step get set committed?)
  (let loop ((c cluster) (wave wave) (round 0))
    (cond
      ((committed? c) round)
      ((null? wave) #f)                          ; quiesced without committing
      (else
       (let inner ((w wave) (c c) (next '()))
         (if (null? w)
             (loop c (reverse next) (+ round 1))
             (let* ((m (car w)) (from (car m)) (to (cadr m)) (msg (caddr m))
                    (res (step (get c to) from msg))
                    (c2 (set c to (car res)))
                    (more (map (lambda (o) (list to (car o) (cdr o))) (cdr res))))
               (inner (cdr w) c2 (append (reverse more) next)))))))))

; ---- Raft: commit rounds with the leader as the write origin (measured) ----
(define (raft-leader-rounds)
  (let* ((c1 (cluster-campaign (cluster-make '(a b c) kv-apply '()) 'a))
         (inj (inject c1 'a (lambda (st) (raft-propose st (list 'set "x" 1)))
                      cluster-get cluster-set))
         (idx (log-len (cluster-get (car inj) 'a))))
    (bfs (car inj) (cdr inj) raft-step cluster-get cluster-set
         (lambda (c) (>= (raft-commit (cluster-get c 'a)) idx)))))

; ---- EPaxos: commit rounds with `origin` as the (co-located) command leader ----
(define (epaxos-origin-rounds origin)
  (let* ((c0 (epx-make '(a b c) kv-interferes? kv-apply '()))
         (inst (cons origin 0))
         (inj (inject c0 origin (lambda (st) (epaxos-propose st (list 'set "x" 1)))
                      epx-get epx-set)))
    (bfs (car inj) (cdr inj) epaxos-step epx-get epx-set
         (lambda (c) (let ((r (cmds-get (epx-get c origin) inst)))
                       (and r (memq (rec-status r) (list 'committed 'executed))))))))

; ============================================================
; results
; ============================================================

(define raft-rt   (raft-leader-rounds))          ; measured: leader round-trip commit
(define epx-rt    (epaxos-origin-rounds 'a))     ; measured: any origin (fast path)
(define raft-fwd  1)                              ; modeled: origin -> leader
(define raft-noti 1)                              ; modeled: leader -> origin (commit ack)
(define raft-nonleader (+ raft-rt raft-fwd raft-noti))

(display "Measured commit rounds (BFS over the real engines):") (newline)
(display "  raft, origin = leader        : ") (display raft-rt) (display " rounds") (newline)
(display "  epaxos, origin = any replica : ") (display epx-rt) (display " rounds") (newline)
(newline)

(display "Commit latency by write origin (3-node cluster), L = one-way delay:") (newline)
(display "  origin       Raft        EPaxos") (newline)
(display "  leader       ") (display raft-rt) (display "L          ") (display epx-rt) (display "L") (newline)
(display "  follower-1   ") (display raft-nonleader) (display "L          ") (display epx-rt) (display "L") (newline)
(display "  follower-2   ") (display raft-nonleader) (display "L          ") (display epx-rt) (display "L") (newline)

; average over uniform origins on a 3-node cluster
(define raft-avg (exact->inexact (/ (+ raft-rt raft-nonleader raft-nonleader) 3)))
(define epx-avg  (exact->inexact epx-rt))
(display "  average      ") (display raft-avg) (display "L      ") (display epx-avg) (display "L") (newline)
(newline)

; concrete WAN example
(define L 50)                                     ; ms, one-way
(display "At L = ") (display L) (display "ms (WAN): mean commit latency  Raft ")
(display (* raft-avg L)) (display "ms   vs   EPaxos ") (display (* epx-avg L)) (display "ms") (newline)
(newline)

; ---- throughput axis: who coordinates? (the no-leader-bottleneck win) ----
(display "Throughput axis — coordination of M writes across a 3-node cluster:") (newline)
(display "  Raft   : the single leader coordinates ALL M  -> bottleneck ~ M") (newline)
(display "  EPaxos : each replica coordinates ~M/3        -> bottleneck ~ M/3 (3x headroom, low conflict)") (newline)
(newline)

; sanity: both engines reach the round-trip commit in 2 rounds
(if (and (= raft-rt 2) (= epx-rt 2))
    (display "ok: both commit in 2 message rounds (1 round trip); EPaxos achieves it from ANY origin")
    (error "unexpected round count" raft-rt epx-rt))
(newline)
