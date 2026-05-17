; T3-E — long-running stateful loop.
;
; Closest of the Tier-3 designs to a real server's hot path:
; per "request", look up a key in a shared hashtable, allocate a
; response record (small vector), update a per-key counter.
;
; One bench iter = REQ_BATCH requests (default 50000). Inner loop
; touches the same hashtable + same key set across all iters so the
; live working set is stable; the per-request allocation churn
; exercises minor-GC behavior under steady-state load.
;
; Reports (via the harness's standard wall+memory shape):
;   - wall_time_seconds.p50 ≈ time for 50k requests
;   - memory.bytes_allocated_total / .alloc_rate_mb_per_sec
;   - memory.gc_* fields (need (gc-stats-enable!) which the harness
;     turns on after warmup)

; --- setup -------------------------------------------------------

(define n-keys 256)
(define req-batch 50000)

; Pre-allocate the key list as a vector of strings. Strings are
; interned-as-allocated each iter — using string equality on the
; same content compares structure, not pointers.
(define keys
  (let ((v (make-vector n-keys "")))
    (let loop ((i 0))
      (if (< i n-keys)
          (begin
            (vector-set! v i
              (string-append "key-" (number->string i)))
            (loop (+ i 1)))
          v))))

; The KV store. Pre-loaded with zero counts so the first batch
; doesn't pay an insert-amortized cost on every request.
(define ht (make-hashtable string-hash string=?))
(let loop ((i 0))
  (if (< i n-keys)
      (begin
        (hashtable-set! ht (vector-ref keys i) 0)
        (loop (+ i 1)))))

; --- per-request work --------------------------------------------

; The "request":
; 1. Hash-lookup the key (no allocation).
; 2. Allocate a 3-field response record (a small vector).
; 3. Update the per-key counter via set!.
;
; Returns the response vector (kept off the hot path so it gets
; promptly collected).
(define (one-request i)
  (let* ((key (vector-ref keys (modulo i n-keys)))
         (count (hashtable-ref ht key 0))
         (response (vector 'ok i count)))
    (hashtable-set! ht key (+ count 1))
    response))

(define (req-loop n)
  (let loop ((i 0) (last #f))
    (if (= i n)
        last
        (loop (+ i 1) (one-request i)))))

; --- harness entry -----------------------------------------------

(realworld-bench
  "t3e-stateful-loop"
  (list (cons (quote n-keys) n-keys)
        (cons (quote req-batch) req-batch))
  (lambda () (req-loop req-batch)))
