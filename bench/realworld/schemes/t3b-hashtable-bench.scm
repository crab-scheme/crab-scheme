; T3-B — hashtable-bench (scaling + key skew).
;
; Three phases per iter:
;   1. INSERT n-keys string keys into a fresh hashtable.
;      Keys are drawn from a fixed pool; duplicates overwrite.
;   2. LOOKUP n-lookups times. Half the lookups are on existing
;      keys, half are "miss" probes. The key distribution is
;      Zipf-like (heavy tail) so a few keys dominate access —
;      mimics real workload skew rather than uniform random.
;   3. DELETE bottom half of keys, lookup the remainder.
;
; The result is the final hashtable size, used as a correctness
; gate (must equal n-keys / 2 after delete).
;
; Phase boundaries aren't separately reported by the harness today
; — the whole iter is one wall-time sample. Per-phase breakouts
; can come in a Phase F enhancement.

(define n-keys 4096)
(define n-lookups 16384)

; Pre-allocate keys + miss-keys outside the bench thunk so the
; per-iter cost is dominated by hashtable work, not key
; construction. Keys are short strings.
(define key-pool
  (let ((v (make-vector n-keys "")))
    (let loop ((i 0))
      (if (< i n-keys)
          (begin
            (vector-set! v i
              (string-append "k-" (number->string i)))
            (loop (+ i 1)))
          v))))

(define miss-pool
  (let ((v (make-vector n-keys "")))
    (let loop ((i 0))
      (if (< i n-keys)
          (begin
            (vector-set! v i
              (string-append "miss-" (number->string i)))
            (loop (+ i 1)))
          v))))

; Zipf-ish access pattern: pick from the first sqrt(n-keys) keys
; ~80 % of the time, the long tail the other 20 %. Coarser than
; a true Zipfian but cheaper to compute and gives the same
; "hot-key skew" property the bench is about.
;
; Picks an index in [0, n-keys) using `seed` as the LCG state.
(define (next-seed s)
  (modulo (+ (* s 1103515245) 12345) 2147483648))

(define hot-keys-count
  (let loop ((n n-keys) (root 1))
    (if (>= (* root root) n) root (loop n (+ root 1)))))

(define (pick-index seed)
  (let ((r (modulo seed 100)))
    (if (< r 80)
        ; hot range
        (modulo seed hot-keys-count)
        ; long tail
        (modulo seed n-keys))))

(define (phase-insert ht)
  (let loop ((i 0))
    (if (< i n-keys)
        (begin
          (hashtable-set! ht (vector-ref key-pool i) i)
          (loop (+ i 1))))))

(define (phase-lookup ht seed)
  (let loop ((i 0) (s seed) (acc 0))
    (if (= i n-lookups)
        acc
        (let* ((s2 (next-seed s))
               (use-miss (= 0 (modulo s 2)))
               (key (if use-miss
                        (vector-ref miss-pool (pick-index s2))
                        (vector-ref key-pool (pick-index s2)))))
          (loop (+ i 1) s2
                (+ acc (hashtable-ref ht key 0)))))))

(define (phase-delete ht)
  (let loop ((i 0))
    (if (< i (quotient n-keys 2))
        (begin
          (hashtable-delete! ht (vector-ref key-pool i))
          (loop (+ i 1))))))

(define (one-iter)
  (let ((ht (make-hashtable string-hash string=?)))
    (phase-insert ht)
    (phase-lookup ht 42)
    (phase-delete ht)
    (hashtable-size ht)))

(realworld-bench
  "t3b-hashtable-bench"
  (list (cons (quote n-keys) n-keys)
        (cons (quote n-lookups) n-lookups))
  (lambda () (one-iter)))
