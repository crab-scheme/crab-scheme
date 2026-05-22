; EPaxos (Egalitarian Paxos) consensus engine — in CrabScheme.
;
; Per CONSTITUTION.md Article I, the protocol is pure dispatch and lives here,
; not in a Rust crate. Leaderless: any replica leads the command it receives.
; Each command occupies an instance (replica . slot) and carries a dependency
; set (interfering instances) + a sequence number that orders cycles.
;
; Article II — pure transitions: (replica, input) -> (replica' . outputs),
; outputs a list of (peer . message). Messages are tagged lists:
;
;   (preaccept       inst command seq deps)
;   (preaccept-reply inst seq deps)
;   (accept          inst command seq deps)
;   (accept-reply    inst)
;   (commit          inst command seq deps)
;
; Flow: PreAccept -> fast path (whole fast quorum returns the leader's seq+deps
; unchanged -> commit) | slow path (Accept the union deps / max seq to a
; majority -> commit). Execution is in dependency order; see `epaxos-execute`.
;
; The networked actor driver is a design-draft (needs cluster send/recv primops,
; like lib/beam/prelude.scm); the pure engine + cluster simulator below are real
; and tested (run lib/consensus/epaxos-kv.scm).

; ============================================================
; generic helpers (this dialect has no rest-args; lists instead)
; ============================================================

(define (aget al k) (cdr (assq k al)))           ; symbol-keyed (eq?)
; Proper (non-growing) replace — bounds node state / collect records to
; O(fields) instead of growing the alist O(transitions).
(define (aset al k v)
  (cond ((null? al) (list (cons k v)))
        ((eq? (caar al) k) (cons (cons k v) (cdr al)))
        (else (cons (car al) (aset (cdr al) k v)))))
(define (aset* al kvs)
  (if (null? kvs) al (aset* (aset al (car kvs) (cadr kvs)) (cddr kvs))))

(define (areplace al k v)                         ; proper replace (equal? keys, iterated)
  (cond ((null? al) (list (cons k v)))
        ((equal? (caar al) k) (cons (cons k v) (cdr al)))
        (else (cons (car al) (areplace (cdr al) k v)))))
(define (adel al k)
  (cond ((null? al) '())
        ((equal? (caar al) k) (adel (cdr al) k))
        (else (cons (car al) (adel (cdr al) k)))))

(define (others id ids)
  (cond ((null? ids) '())
        ((eqv? (car ids) id) (others id (cdr ids)))
        (else (cons (car ids) (others id (cdr ids))))))

; set-as-list (equal? membership)
(define (set-add s x) (if (member x s) s (cons x s)))
(define (set-union a b) (if (null? b) a (set-union (set-add a (car b)) (cdr b))))

; insertion sort by a binary less? predicate
(define (insert-sorted less? x lst)
  (cond ((null? lst) (list x))
        ((less? x (car lst)) (cons x lst))
        (else (cons (car lst) (insert-sorted less? x (cdr lst))))))
(define (isort less? lst)
  (if (null? lst) '() (insert-sorted less? (car lst) (isort less? (cdr lst)))))

; ============================================================
; per-instance record + replica state
; ============================================================

(define (mk-rec command seq deps status) (list command seq deps status))
(define (rec-command r) (list-ref r 0))
(define (rec-seq r)     (list-ref r 1))
(define (rec-deps r)    (list-ref r 2))
(define (rec-status r)  (list-ref r 3))

(define (make-epaxos id replicas interferes apply-fn sm0)
  (list (cons 'id id) (cons 'replicas replicas) (cons 'next-slot 0)
        (cons 'cmds '()) (cons 'collects '())
        (cons 'interferes interferes) (cons 'apply apply-fn)
        (cons 'sm sm0) (cons 'executed '())))

(define (epaxos-id st) (aget st 'id))
(define (epaxos-sm st) (aget st 'sm))
(define (epaxos-executed st) (aget st 'executed))   ; instances in execution order

(define (cmds-get st inst) (let ((p (assoc inst (aget st 'cmds)))) (if p (cdr p) #f)))
(define (cmds-set st inst rec) (aset st 'cmds (areplace (aget st 'cmds) inst rec)))
(define (collect-get st inst) (let ((p (assoc inst (aget st 'collects)))) (if p (cdr p) #f)))
(define (collect-set st inst c) (aset st 'collects (areplace (aget st 'collects) inst c)))
(define (collect-del st inst) (aset st 'collects (adel (aget st 'collects) inst)))

(define (peers st) (others (aget st 'id) (aget st 'replicas)))
(define (bcast st msg) (map (lambda (p) (cons p msg)) (peers st)))

(define (rcount st) (length (aget st 'replicas)))
(define (slow-quorum st) (+ 1 (quotient (rcount st) 2)))
; EPaxos fast quorum: F + ceil(F/2), N = 2F+1.
(define (fast-quorum st)
  (let* ((f (quotient (- (rcount st) 1) 2)))
    (+ f (quotient (+ f 1) 2))))

; (seq, deps) for `command` against every known interfering instance but `exclude`
(define (deps-and-seq st command exclude)
  (let loop ((al (aget st 'cmds)) (deps '()) (mx 0))
    (if (null? al)
        (cons (+ mx 1) deps)
        (let ((inst (caar al)) (other (cdar al)))
          (if (or (equal? inst exclude)
                  (not ((aget st 'interferes) command (rec-command other))))
              (loop (cdr al) deps mx)
              (loop (cdr al) (set-add deps inst) (max mx (rec-seq other))))))))

; ============================================================
; transitions: each returns (replica' . outputs)
; ============================================================

(define (epaxos-propose st command)
  (let* ((id (aget st 'id)) (slot (aget st 'next-slot))
         (inst (cons id slot))
         (sd (deps-and-seq st command inst))
         (seq (car sd)) (deps (cdr sd))
         (st (aset st 'next-slot (+ slot 1)))
         (st (cmds-set st inst (mk-rec command seq deps 'preaccepted)))
         (st (collect-set st inst
                          (list (cons 'command command) (cons 'iseq seq) (cons 'ideps deps)
                                (cons 'replies 1) (cons 'agreed 1)
                                (cons 'useq seq) (cons 'udeps deps)
                                (cons 'accepting #f) (cons 'oks 1)))))
    (cons st (bcast st (list 'preaccept inst command seq deps)))))

(define (epaxos-step st from msg)
  (case (car msg)
    ((preaccept)       (on-preaccept st from msg))
    ((preaccept-reply) (on-preaccept-reply st from msg))
    ((accept)          (on-accept st from msg))
    ((accept-reply)    (on-accept-reply st from msg))
    ((commit)          (on-commit st from msg))
    (else              (cons st '()))))

(define (on-preaccept st from msg)
  (let* ((inst (list-ref msg 1)) (command (list-ref msg 2))
         (seq (list-ref msg 3)) (deps (list-ref msg 4))
         (sd (deps-and-seq st command inst))
         (mseq (max seq (car sd)))
         (mdeps (set-union deps (cdr sd)))
         (st (cmds-set st inst (mk-rec command mseq mdeps 'preaccepted))))
    (cons st (list (cons from (list 'preaccept-reply inst mseq mdeps))))))

(define (on-preaccept-reply st from msg)
  (let* ((inst (list-ref msg 1)) (seq (list-ref msg 2)) (deps (list-ref msg 3))
         (col (collect-get st inst)))
    (if (or (not col) (aget col 'accepting))
        (cons st '())
        (let* ((replies (+ 1 (aget col 'replies)))
               (agreed (+ (aget col 'agreed)
                          (if (and (= seq (aget col 'iseq)) (equal? deps (aget col 'ideps))) 1 0)))
               (useq (max (aget col 'useq) seq))
               (udeps (set-union (aget col 'udeps) deps))
               (col (aset* col (list 'replies replies 'agreed agreed 'useq useq 'udeps udeps)))
               (st (collect-set st inst col)))
          (cond
            ((< replies (fast-quorum st)) (cons st '()))
            ((>= agreed (fast-quorum st))
             (commit-leader st inst (aget col 'command) (aget col 'iseq) (aget col 'ideps)))
            (else
             (let* ((col (aset col 'accepting #t))
                    (st (collect-set st inst col))
                    (st (cmds-set st inst (mk-rec (aget col 'command) useq udeps 'accepted))))
               (cons st (bcast st (list 'accept inst (aget col 'command) useq udeps))))))))))

(define (on-accept st from msg)
  (let* ((inst (list-ref msg 1)) (command (list-ref msg 2))
         (seq (list-ref msg 3)) (deps (list-ref msg 4))
         (st (cmds-set st inst (mk-rec command seq deps 'accepted))))
    (cons st (list (cons from (list 'accept-reply inst))))))

(define (on-accept-reply st from msg)
  (let* ((inst (list-ref msg 1)) (col (collect-get st inst)))
    (if (or (not col) (not (aget col 'accepting)))
        (cons st '())
        (let* ((oks (+ 1 (aget col 'oks)))
               (col (aset col 'oks oks))
               (st (collect-set st inst col)))
          (if (>= oks (slow-quorum st))
              (commit-leader st inst (aget col 'command) (aget col 'useq) (aget col 'udeps))
              (cons st '()))))))

(define (commit-leader st inst command seq deps)
  (let* ((st (collect-del st inst))
         (st (cmds-set st inst (mk-rec command seq deps 'committed)))
         (st (epaxos-execute st)))
    (cons st (bcast st (list 'commit inst command seq deps)))))

(define (on-commit st from msg)
  (let* ((inst (list-ref msg 1)) (command (list-ref msg 2))
         (seq (list-ref msg 3)) (deps (list-ref msg 4))
         (st (cmds-set st inst (mk-rec command seq deps 'committed))))
    (cons (epaxos-execute st) '())))

; ============================================================
; execution: dependency order (Article II)
; ============================================================
;
; EPaxos's seq increases along dependency edges and ties within a cycle break
; by (seq, instance), so a global sort by (seq, instance) over the executable
; set is a valid, replica-consistent execution order. (The Rust prototype used
; full Tarjan SCC; this is the equivalent for the dependency shapes that arise.)
; An instance is executable only once every transitive dependency is committed —
; computed as a fixpoint.

(define (committed-not-executed st)
  (let loop ((al (aget st 'cmds)) (acc '()))
    (cond ((null? al) acc)
          ((eq? (rec-status (cdar al)) 'committed) (loop (cdr al) (cons (caar al) acc)))
          (else (loop (cdr al) acc)))))

(define (dep-ok? st dep elig)
  (let ((r (cmds-get st dep)))
    (or (and r (eq? (rec-status r) 'executed))
        (member dep elig))))

(define (deps-ok? st inst elig)
  (let loop ((ds (rec-deps (cmds-get st inst))))
    (cond ((null? ds) #t)
          ((dep-ok? st (car ds) elig) (loop (cdr ds)))
          (else #f))))

(define (filter-elig st elig)
  (let loop ((es elig) (acc '()))
    (cond ((null? es) (reverse acc))
          ((deps-ok? st (car es) elig) (loop (cdr es) (cons (car es) acc)))
          (else (loop (cdr es) acc)))))

(define (fixpoint-elig st elig)
  (let ((next (filter-elig st elig)))
    (if (= (length next) (length elig)) elig (fixpoint-elig st next))))

(define (inst-key<? i j)                          ; (replica . slot) total order
  (let ((ri (symbol->string (car i))) (rj (symbol->string (car j))))
    (cond ((string<? ri rj) #t)
          ((string>? ri rj) #f)
          (else (< (cdr i) (cdr j))))))
(define (inst<? st i j)
  (let ((si (rec-seq (cmds-get st i))) (sj (rec-seq (cmds-get st j))))
    (cond ((< si sj) #t) ((> si sj) #f) (else (inst-key<? i j)))))

(define (epaxos-execute st)
  (let* ((elig (fixpoint-elig st (committed-not-executed st)))
         (ordered (isort (lambda (i j) (inst<? st i j)) elig)))
    (let loop ((os ordered) (st st))
      (if (null? os) st
          (let* ((inst (car os)) (r (cmds-get st inst)))
            (if (eq? (rec-status r) 'executed)
                (loop (cdr os) st)
                (let* ((sm2 ((aget st 'apply) (aget st 'sm) (rec-command r)))
                       (st (aset st 'sm sm2))
                       (st (cmds-set st inst (mk-rec (rec-command r) (rec-seq r) (rec-deps r) 'executed)))
                       (st (aset st 'executed (append (aget st 'executed) (list inst)))))
                  (loop (cdr os) st))))))))

; ============================================================
; deterministic in-Scheme cluster simulator
; ============================================================

(define (epx-make ids interferes apply-fn sm0)
  (map (lambda (id) (cons id (make-epaxos id ids interferes apply-fn sm0))) ids))
(define (epx-get c id) (cdr (assq id c)))
(define (epx-set c id st) (areplace c id st))

(define (epx-settle c queue)
  (if (null? queue) c
      (let* ((m (car queue)) (from (car m)) (to (cadr m)) (msg (caddr m))
             (res (epaxos-step (epx-get c to) from msg))
             (c2 (epx-set c to (car res)))
             (more (map (lambda (o) (list to (car o) (cdr o))) (cdr res))))
        (epx-settle c2 (append (cdr queue) more)))))

; Run an action (e.g. propose) on one node, returning (cluster . pending-queue)
; WITHOUT settling — so several proposes can be injected "concurrently" before
; any message is delivered, then settled together.
(define (epx-inject c id action)
  (let* ((res (action (epx-get c id)))
         (c2 (epx-set c id (car res)))
         (q (map (lambda (o) (list id (car o) (cdr o))) (cdr res))))
    (cons c2 q)))
