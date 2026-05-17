; T3-F — long-soak stability test.
;
; The leak-detection bench. Designed to run for minutes (default
; budget: 1800 s = 30 min via REALWORLD_TIME_BUDGET_SEC=1800) on a
; workload with stable working-set semantics. If the implementation
; isn't leaking, RSS oscillates around a fixed baseline after
; warmup. If it IS leaking, RSS climbs monotonically.
;
; The workload is a tighter version of t3e-stateful-loop: a fixed
; KV store with a bounded key set, per-iter request batch that
; allocates throwaway response records. Crucially, NOTHING the
; iter does should accumulate new live references — the hashtable
; is pre-populated once, every response vector is dropped at the
; iter boundary.
;
; Per-iter footprint:
;   - 1 hashtable read per request
;   - 1 vector(3) alloc per request (the response — dropped)
;   - 1 hashtable update (overwrites existing slot, no new bucket)
;
; Expected behavior on a non-leaking implementation:
;   - peak_rss_bytes stays within ~50 % of post-warmup RSS
;   - rss_growth_bytes stays small (< 1 MB / 1000 iters)
;   - bytes_allocated_total climbs monotonically (every iter alloc'd
;     response vectors), which is correct — cumulative is global
;
; Run with the default 60s budget for a smoke test; bump to 1800
; for the real soak:
;
;   REALWORLD_TIME_BUDGET_SEC=1800 \
;     bench/realworld/runner.sh --bench t3f-soak --tier vm \
;       --warmup 5 --measure 999999 \
;       --output bench/realworld/results/soak.jsonl

(define n-keys 128)
(define req-batch 10000)

; Pre-allocate key list outside the bench so per-iter cost is the
; KV ops, not key construction.
(define keys
  (let ((v (make-vector n-keys "")))
    (let loop ((i 0))
      (if (< i n-keys)
          (begin
            (vector-set! v i
              (string-append "soak-" (number->string i)))
            (loop (+ i 1)))
          v))))

; KV store, pre-loaded with zero counts.
(define ht (make-hashtable string-hash string=?))
(let loop ((i 0))
  (if (< i n-keys)
      (begin
        (hashtable-set! ht (vector-ref keys i) 0)
        (loop (+ i 1)))))

(define (one-request i)
  (let* ((key (vector-ref keys (modulo i n-keys)))
         (count (hashtable-ref ht key 0))
         (response (vector 'soak i count)))
    (hashtable-set! ht key (+ count 1))
    response))

(define (req-loop n)
  (let loop ((i 0) (last #f))
    (if (= i n)
        last
        (loop (+ i 1) (one-request i)))))

(realworld-bench
  "t3f-soak"
  (list (cons (quote n-keys) n-keys)
        (cons (quote req-batch) req-batch))
  (lambda () (req-loop req-batch)))
