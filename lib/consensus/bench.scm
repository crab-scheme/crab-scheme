; Throughput micro-benchmark for the Scheme KV cache on both engines.
;
;   crabscheme run lib/consensus/bench.scm
;
; HONEST SCOPE (Constitution Article III/VI): this measures the *protocol-logic
; compute cost* of committing writes through the deterministic in-process
; cluster simulator, in the CrabScheme tree-walker. There is NO real network,
; NO disk/fsync, NO concurrency. So these are not comparable to production
; distributed-KV throughput; the fair comparison here is Raft-vs-EPaxos on the
; same harness. (The draft also uses association lists — O(n) per op — so
; absolute numbers fall off with N; a production state machine would use a hash
; table.)

(include "lib/consensus/raft.scm")
(include "lib/consensus/epaxos.scm")

; ---- KV state machine + interference ----
(define (kv-del al k)
  (cond ((null? al) '())
        ((equal? (caar al) k) (kv-del (cdr al) k))
        (else (cons (car al) (kv-del (cdr al) k)))))
(define (kv-set al k v) (cons (cons k v) (kv-del al k)))
(define (kv-apply state op)
  (case (car op)
    ((set) (kv-set state (cadr op) (caddr op)))
    ((del) (kv-del state (cadr op)))
    (else  state)))
(define (kv-interferes? a b) (equal? (cadr a) (cadr b)))

; ---- timing ----
(define (now) (current-jiffy))
(define (secs t0 t1) (exact->inexact (/ (- t1 t0) (jiffies-per-second))))
(define (key i) (string-append "k" (number->string i)))

; ---- benchmarks: each returns elapsed seconds for N committed writes ----

(define (bench-raft n)
  (let* ((c0 (cluster-make '(a b c) kv-apply '()))
         (c1 (cluster-campaign c0 'a))
         (t0 (now)))
    (let loop ((i 0) (c c1))
      (if (>= i n) (secs t0 (now))
          (loop (+ i 1) (cluster-propose c 'a (list 'set (key i) i)))))))

(define (bench-epaxos-distinct n)        ; non-conflicting → fast path, no deps
  (let* ((c0 (epx-make '(a b c) kv-interferes? kv-apply '()))
         (t0 (now)))
    (let loop ((i 0) (c c0))
      (if (>= i n) (secs t0 (now))
          (let* ((leader (list-ref '(a b c) (modulo i 3)))
                 (inj (epx-inject c leader (lambda (st) (epaxos-propose st (list 'set (key i) i))))))
            (loop (+ i 1) (epx-settle (car inj) (cdr inj))))))))

(define (bench-epaxos-conflict n)        ; all same key → deps accumulate
  (let* ((c0 (epx-make '(a b c) kv-interferes? kv-apply '()))
         (t0 (now)))
    (let loop ((i 0) (c c0))
      (if (>= i n) (secs t0 (now))
          (let* ((leader (list-ref '(a b c) (modulo i 3)))
                 (inj (epx-inject c leader (lambda (st) (epaxos-propose st (list 'set "hot" i))))))
            (loop (+ i 1) (epx-settle (car inj) (cdr inj))))))))

; ---- #2: O(1) hash-table state machine (R6RS mutable hashtable) ----
; Removes the KV state machine's O(n) cost. A mutable hashtable is NOT a pure
; value (Article II), so it's used here in the benchmark only — with a FRESH
; table per replica — while the library KV (kv-cache.scm) stays pure (alist).
; This isolates how much of the falloff is the state machine vs the engine's own
; alist/list internals.
(define (kv-apply-ht ht op)
  (case (car op)
    ((set) (hashtable-set! ht (cadr op) (caddr op)) ht)
    ((del) (hashtable-delete! ht (cadr op)) ht)
    (else ht)))
(define (fresh-ht) (make-hashtable equal-hash equal?))

(define (bench-raft-ht n)
  (let* ((ids '(a b c))
         (c0 (map (lambda (id) (cons id (make-raft id ids kv-apply-ht (fresh-ht)))) ids))
         (c1 (cluster-campaign c0 'a))
         (t0 (now)))
    (let loop ((i 0) (c c1))
      (if (>= i n) (secs t0 (now))
          (loop (+ i 1) (cluster-propose c 'a (list 'set (key i) i)))))))

(define (bench-epaxos-distinct-ht n)
  (let* ((ids '(a b c))
         (c0 (map (lambda (id) (cons id (make-epaxos id ids kv-interferes? kv-apply-ht (fresh-ht)))) ids))
         (t0 (now)))
    (let loop ((i 0) (c c0))
      (if (>= i n) (secs t0 (now))
          (let* ((leader (list-ref '(a b c) (modulo i 3)))
                 (inj (epx-inject c leader (lambda (st) (epaxos-propose st (list 'set (key i) i))))))
            (loop (+ i 1) (epx-settle (car inj) (cdr inj))))))))

(define (report label n s)
  (display "  ") (display label) (display ": ") (display n)
  (display " writes in ") (display s) (display "s  =>  ")
  (display (round (/ n s))) (display " writes/sec") (newline))

(display "=== pure alist state machine ===") (newline)
(for-each
 (lambda (n)
   (display "N = ") (display n) (newline)
   (report "raft   (distinct keys)        " n (bench-raft n))
   (report "epaxos (distinct, fast-path)  " n (bench-epaxos-distinct n))
   (report "epaxos (same key, conflicting)" n (bench-epaxos-conflict n))
   (newline))
 '(20 40 80))

(display "=== O(1) hash-table state machine (bench-only; engine internals unchanged) ===") (newline)
(for-each
 (lambda (n)
   (display "N = ") (display n) (newline)
   (report "raft   (distinct keys, ht)    " n (bench-raft-ht n))
   (report "epaxos (distinct, fast-path,ht)" n (bench-epaxos-distinct-ht n))
   (newline))
 '(20 40 80))
