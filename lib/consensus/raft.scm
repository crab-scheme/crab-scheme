; Raft consensus engine — in CrabScheme.
;
; Per CONSTITUTION.md Article I (the code is Scheme; Rust is the machine),
; the consensus PROTOCOL is pure dispatch and lives here, not in a Rust crate.
; Only the transport (cs-net Channel::Consensus) and actors (cs-actor) are Rust
; primitives.
;
; Article II — this engine is PURE: every transition is
;   (node, input) -> (node' . outputs)
; with no clocks, sockets, or mutation. `outputs` is a list of (peer . message).
; A node value is an association list; messages are tagged lists:
;
;   (rv  term cand last-idx last-term)        ; RequestVote
;   (rvr term granted)                        ; RequestVote reply
;   (ae  term leader prev-idx prev-term entries leader-commit)  ; AppendEntries
;   (aer term success match-idx)              ; AppendEntries reply
;
; A log entry is (term . command). The state machine is a pure function
; (apply-fn state command) -> state'.
;
; The networked driver (spawn a loop that ticks on a timer + steps on
; raw-receive, sending outputs over cs-net) is a design-draft sketched at the
; bottom — it needs cluster send/recv primops not yet wired (same status as
; lib/beam/prelude.scm).

; ============================================================
; assoc-list node helpers (immutable, shadow-update)
; ============================================================

(define (aget al k) (cdr (assq k al)))
(define (aset al k v) (cons (cons k v) al))      ; shadows; assq finds newest
; aset* takes a flat list (k v k v ...) — this dialect has no rest-args.
(define (aset* al kvs)
  (if (null? kvs) al (aset* (aset al (car kvs) (cadr kvs)) (cddr kvs))))

(define (others id ids)                          ; ids minus id  -> peers
  (cond ((null? ids) '())
        ((eqv? (car ids) id) (others id (cdr ids)))
        (else (cons (car ids) (others id (cdr ids))))))

(define (take-n lst n)                            ; first n elements
  (if (or (<= n 0) (null? lst)) '()
      (cons (car lst) (take-n (cdr lst) (- n 1)))))

; ============================================================
; node construction + accessors
; ============================================================

(define (make-raft id ids apply-fn sm0)
  (list (cons 'id id) (cons 'peers (others id ids)) (cons 'all ids)
        (cons 'role 'follower) (cons 'term 0) (cons 'voted-for #f)
        (cons 'log '()) (cons 'commit 0) (cons 'applied 0) (cons 'votes '())
        (cons 'next '()) (cons 'match '()) (cons 'apply apply-fn) (cons 'sm sm0)))

(define (raft-id st)      (aget st 'id))
(define (raft-role st)    (aget st 'role))
(define (raft-leader? st) (eq? (aget st 'role) 'leader))
(define (raft-term st)    (aget st 'term))
(define (raft-commit st)  (aget st 'commit))
(define (raft-sm st)      (aget st 'sm))

; ---- log helpers (1-based) ----
(define (log-len st) (length (aget st 'log)))
(define (entry-term st i)
  (if (<= i 0) 0 (car (list-ref (aget st 'log) (- i 1)))))
(define (last-log-term st) (entry-term st (log-len st)))
(define (entries-from st i) (list-tail (aget st 'log) (- i 1)))   ; i in 1..len+1

(define (majority st) (+ 1 (quotient (length (aget st 'all)) 2)))

; ============================================================
; leader replication helpers
; ============================================================

(define (append-for st peer)
  (let* ((nx (cdr (assq peer (aget st 'next))))
         (prev (- nx 1)))
    (list 'ae (aget st 'term) (aget st 'id) prev (entry-term st prev)
          (entries-from st nx) (aget st 'commit))))

(define (broadcast-append st)
  (cons st (map (lambda (p) (cons p (append-for st p))) (aget st 'peers))))

(define (become-leader st)
  (let* ((nx (+ 1 (log-len st)))
         (st (aset* st (list 'role 'leader
                             'next (map (lambda (p) (cons p nx)) (aget st 'peers))
                             'match (map (lambda (p) (cons p 0)) (aget st 'peers))))))
    (broadcast-append st)))

; ============================================================
; commit + apply
; ============================================================

(define (count-acks match peers n)
  (if (null? peers) 0
      (+ (if (>= (cdr (assq (car peers) match)) n) 1 0)
         (count-acks match (cdr peers) n))))

(define (apply-committed st)
  (let loop ((st st))
    (if (>= (aget st 'applied) (aget st 'commit)) st
        (let* ((i (+ 1 (aget st 'applied)))
               (cmd (cdr (list-ref (aget st 'log) (- i 1))))
               (sm2 ((aget st 'apply) (aget st 'sm) cmd)))
          (loop (aset* st (list 'applied i 'sm sm2)))))))

; Leader: advance commit to the highest index replicated on a quorum AND from
; the current term (Raft §5.4.2), then apply.
(define (maybe-commit st)
  (let loop ((n (log-len st)))
    (cond
      ((<= n (aget st 'commit)) st)
      ((and (= (entry-term st n) (aget st 'term))
            (>= (+ 1 (count-acks (aget st 'match) (aget st 'peers) n)) (majority st)))
       (apply-committed (aset st 'commit n)))
      (else (loop (- n 1))))))

; ============================================================
; public transitions: each returns (node' . outputs)
; ============================================================

(define (raft-campaign st)
  (let* ((term (+ 1 (aget st 'term)))
         (id (aget st 'id))
         (st (aset* st (list 'role 'candidate 'term term 'voted-for id 'votes (list id)))))
    (if (>= (length (aget st 'votes)) (majority st))
        (become-leader st)                       ; single-node: instant majority
        (cons st (map (lambda (p)
                        (cons p (list 'rv term id (log-len st) (last-log-term st))))
                      (aget st 'peers))))))

(define (raft-propose st command)
  (if (not (raft-leader? st))
      (cons st '())
      (broadcast-append
       (aset st 'log (append (aget st 'log) (list (cons (aget st 'term) command)))))))

(define (raft-tick st)
  (if (raft-leader? st) (broadcast-append st) (cons st '())))

(define (raft-step st from msg)
  (case (car msg)
    ((rv)  (on-rv st msg))
    ((rvr) (on-rvr st from msg))
    ((ae)  (on-ae st msg))
    ((aer) (on-aer st from msg))
    (else  (cons st '()))))

(define (on-rv st msg)
  (let* ((term (list-ref msg 1)) (cand (list-ref msg 2))
         (cidx (list-ref msg 3)) (cterm (list-ref msg 4))
         (st (if (> term (aget st 'term))
                 (aset* st (list 'term term 'role 'follower 'voted-for #f)) st))
         (up (or (> cterm (last-log-term st))
                 (and (= cterm (last-log-term st)) (>= cidx (log-len st)))))
         (grant (and (= term (aget st 'term))
                     (or (not (aget st 'voted-for)) (eqv? (aget st 'voted-for) cand))
                     up))
         (st (if grant (aset st 'voted-for cand) st)))
    (cons st (list (cons cand (list 'rvr (aget st 'term) grant))))))

(define (on-rvr st from msg)
  (let ((term (list-ref msg 1)) (granted (list-ref msg 2)))
    (cond
      ((> term (aget st 'term))
       (cons (aset* st (list 'term term 'role 'follower 'voted-for #f)) '()))
      ((and (eq? (aget st 'role) 'candidate) (= term (aget st 'term)) granted)
       (let* ((votes (if (memv from (aget st 'votes)) (aget st 'votes)
                         (cons from (aget st 'votes))))
              (st (aset st 'votes votes)))
         (if (>= (length votes) (majority st)) (become-leader st) (cons st '()))))
      (else (cons st '())))))

(define (on-ae st msg)
  (let ((term (list-ref msg 1)) (leader (list-ref msg 2))
        (pidx (list-ref msg 3)) (pterm (list-ref msg 4))
        (entries (list-ref msg 5)) (lc (list-ref msg 6)))
    (if (< term (aget st 'term))
        (cons st (list (cons leader (list 'aer (aget st 'term) #f 0))))
        (let* ((st (aset* st (list 'term term 'role 'follower)))
               (ok (and (<= pidx (log-len st)) (= (entry-term st pidx) pterm))))
          (if (not ok)
              (cons st (list (cons leader (list 'aer (aget st 'term) #f 0))))
              (let* ((kept (take-n (aget st 'log) pidx))
                     (newlog (append kept entries))
                     (midx (+ pidx (length entries)))
                     (st (aset st 'log newlog))
                     (st (if (> lc (aget st 'commit))
                             (apply-committed (aset st 'commit (min lc (length newlog))))
                             st)))
                (cons st (list (cons leader (list 'aer (aget st 'term) #t midx))))))))))

(define (on-aer st from msg)
  (let ((term (list-ref msg 1)) (succ (list-ref msg 2)) (midx (list-ref msg 3)))
    (cond
      ((> term (aget st 'term))
       (cons (aset* st (list 'term term 'role 'follower 'voted-for #f)) '()))
      ((not (and (raft-leader? st) (= term (aget st 'term)))) (cons st '()))
      (succ
       (let ((st (aset* st (list 'match (aset (aget st 'match) from midx)
                                 'next (aset (aget st 'next) from (+ midx 1))))))
         (cons (maybe-commit st) '())))
      (else
       (let* ((nx (cdr (assq from (aget st 'next))))
              (st (aset st 'next (aset (aget st 'next) from (max 1 (- nx 1))))))
         (cons st (list (cons from (append-for st from)))))))))

; ============================================================
; deterministic in-Scheme cluster simulator (Article III: prove it)
; ============================================================
;
; A cluster is an alist (id . node). It routes outputs to quiescence with full
; control over delivery — no tokio, no sockets, no wall clock.

(define (cluster-make ids apply-fn sm0)
  (map (lambda (id) (cons id (make-raft id ids apply-fn sm0))) ids))

(define (cluster-get c id) (cdr (assq id c)))
(define (cluster-set c id st) (cons (cons id st) c))    ; shadow; assq newest

; Deliver every queued (from to msg) — and the replies they beget — until none
; remain. Returns the settled cluster.
(define (cluster-settle c queue)
  (if (null? queue) c
      (let* ((m (car queue)) (from (car m)) (to (cadr m)) (msg (caddr m))
             (res (raft-step (cluster-get c to) from msg))
             (c2 (cluster-set c to (car res)))
             (more (map (lambda (o) (list to (car o) (cdr o))) (cdr res))))
        (cluster-settle c2 (append (cdr queue) more)))))

; Run an action (campaign / propose / tick) on one node, then settle.
(define (cluster-drive c id action)
  (let* ((res (action (cluster-get c id)))
         (c2 (cluster-set c id (car res)))
         (q (map (lambda (o) (list id (car o) (cdr o))) (cdr res))))
    (cluster-settle c2 q)))

(define (cluster-campaign c id) (cluster-drive c id raft-campaign))
(define (cluster-propose c id cmd) (cluster-drive c id (lambda (st) (raft-propose st cmd))))
(define (cluster-tick c id) (cluster-drive c id raft-tick))

; ============================================================
; networked driver — DESIGN-DRAFT (needs primops, not yet wired)
; ============================================================
;
; Once cs-runtime exposes the cluster send/recv primops (M02 tail) alongside the
; cs-actor primops (spawn/send/raw-receive/self), a node runs as an actor that
; pumps the SAME pure transitions:
;
;   (define (raft-actor st0 tick-ms)
;     (spawn
;       (lambda ()
;         (let loop ((st st0))
;           (let ((msg (raw-receive tick-ms)))           ; cluster message or timeout
;             (let ((res (if (eq? msg '*timeout*)
;                            (raft-tick st)
;                            (raft-step st (msg-from msg) (msg-body msg)))))
;               (for-each (lambda (o) (cluster-send (car o) (cdr o))) (cdr res))
;               (loop (car res))))))))
;
; `cluster-send` / the inbound framing ride cs-net's Channel::Consensus. Until
; those primops land this is illustrative only — the pure engine above is the
; part that is real and tested.
