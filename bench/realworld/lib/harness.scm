; Real-world benchmark harness.
;
; Provides (realworld-bench name params thunk): runs thunk through
; warmup + measurement iterations, captures wall-time + memory
; deltas via the Phase B GC primops, and emits one line of JSON to
; stdout matching docs/research/realworld_benchmarks_spec.md.
;
; Config via environment variables (defaults in parens):
;   REALWORLD_WARMUP_ITERS   (3)
;   REALWORLD_MEASURE_ITERS  (10)
;   REALWORLD_TIME_BUDGET_SEC (60)
;   REALWORLD_ENGINE         ("crabscheme")
;   REALWORLD_ENGINE_TIER    ("walker")
;   REALWORLD_ENGINE_VERSION ("dev")
;
; Per-iter loop stops at the first of MEASURE_ITERS reached OR
; TIME_BUDGET_SEC elapsed. Short benches get high iter counts and
; tight percentile statistics; long benches get fewer iters but
; each runs full duration.

; ---- env helpers ------------------------------------------------

(define (rw-env name)
  (get-environment-variable name))

(define (rw-env-num name default)
  (let ((v (rw-env name)))
    (if v
        (or (string->number v) default)
        default)))

(define (rw-env-str name default)
  (or (rw-env name) default))

; ---- JSON emission ----------------------------------------------
;
; Streams JSON directly via display rather than building strings.
; Adequate for a single-line single-object emit; not a general
; JSON serializer.

(define (jstr s)
  (display "\"")
  (display s)  ; assume s has no embedded quotes / control chars
  (display "\""))

(define (jfield key val-thunk)
  (jstr key)
  (display ":")
  (val-thunk))

(define (jcomma) (display ","))

(define (jobj-of fields)
  (display "{")
  (let loop ((rest fields) (first? #t))
    (if (null? rest)
        (display "}")
        (begin
          (if (not first?) (jcomma))
          (let ((p (car rest)))
            (jstr (car p))
            (display ":")
            ((cdr p)))
          (loop (cdr rest) #f)))))

(define (jlist-of items)
  (display "[")
  (let loop ((rest items) (first? #t))
    (if (null? rest)
        (display "]")
        (begin
          (if (not first?) (jcomma))
          ((car rest))
          (loop (cdr rest) #f)))))

(define (jnum n) (lambda () (display n)))
(define (jstr-v s) (lambda () (jstr s)))
(define (jbool b) (lambda () (display (if b "true" "false"))))

; ---- stats helpers ----------------------------------------------

(define (alist-get key alist)
  (let loop ((rest alist))
    (cond ((null? rest) #f)
          ((eq? (car (car rest)) key) (cdr (car rest)))
          (else (loop (cdr rest))))))

(define (rw-sum xs)
  (let loop ((rest xs) (acc 0))
    (if (null? rest) acc (loop (cdr rest) (+ acc (car rest))))))

(define (rw-min xs)
  (let loop ((rest (cdr xs)) (acc (car xs)))
    (if (null? rest) acc
        (loop (cdr rest) (if (< (car rest) acc) (car rest) acc)))))

(define (rw-max xs)
  (let loop ((rest (cdr xs)) (acc (car xs)))
    (if (null? rest) acc
        (loop (cdr rest) (if (> (car rest) acc) (car rest) acc)))))

(define (rw-mean xs)
  (exact->inexact (/ (rw-sum xs) (length xs))))

; Percentile via linear interpolation between adjacent ranks.
; xs must be already sorted ascending.
(define (rw-percentile xs p)
  (let* ((n (length xs))
         (rank (* p (- n 1)))
         (lo (exact (floor rank)))
         (hi (exact (ceiling rank)))
         (frac (- rank lo))
         (lo-v (list-ref xs lo))
         (hi-v (list-ref xs hi)))
    (+ lo-v (* frac (- hi-v lo-v)))))

(define (rw-stddev xs)
  (let* ((m (rw-mean xs))
         (n (length xs))
         (sq-sum (rw-sum (map (lambda (x) (let ((d (- x m))) (* d d))) xs))))
    (sqrt (/ sq-sum (max 1 (- n 1))))))

; ---- main entry point -------------------------------------------

(define (realworld-bench name params thunk)
  ; Read config once.
  (let ((warmup (rw-env-num "REALWORLD_WARMUP_ITERS" 3))
        (max-iters (rw-env-num "REALWORLD_MEASURE_ITERS" 10))
        (budget-sec (rw-env-num "REALWORLD_TIME_BUDGET_SEC" 60))
        (engine (rw-env-str "REALWORLD_ENGINE" "crabscheme"))
        (engine-tier (rw-env-str "REALWORLD_ENGINE_TIER" "walker"))
        (engine-version (rw-env-str "REALWORLD_ENGINE_VERSION" "dev"))
        (jiffies-per-sec (jiffies-per-second)))
    ; Warmup phase — discard.
    (let warm ((i 0))
      (if (< i warmup) (begin (thunk) (warm (+ i 1)))))
    ; Reset + enable stats for the measurement window.
    ;
    ; Note: auto-collect is enabled but doesn't yet fire from the
    ; runtime's hot allocations — most values go through cs_gc::Gc::new
    ; (the unregistered constructor used by Pair::new, Hashtable::new,
    ; etc. while the heap-rooting migration is in progress), which
    ; doesn't bump the heap's rolling alloc_count that drives the
    ; auto-collect threshold. As a result GC% and max-pause stay at 0
    ; in the harness output. The bytes_allocated_total and
    ; alloc_rate_mb_per_sec columns remain meaningful — they pull
    ; from the global counter Gc::new does bump. Real GC numbers will
    ; surface once the migration completes.
    ;
    ; Benches that want to exercise the GC code path today can call
    ; (collect-garbage) inside their thunk.
    (gc-auto-collect-enable!)
    (gc-set-threshold! 262144)
    (gc-stats-enable!)
    (gc-stats-reset!)
    (let* ((budget-jiffies (* budget-sec jiffies-per-sec))
           (deadline-jiffy (+ (current-jiffy) budget-jiffies)))
      ; Measurement loop. Stops at iter cap OR time budget,
      ; whichever first.
      (let loop ((i 0) (wall-samples '()) (byte-samples '()))
        (if (or (>= i max-iters) (>= (current-jiffy) deadline-jiffy))
            (emit-result name params engine engine-tier engine-version
                         (reverse wall-samples) (reverse byte-samples))
            (let ((b0 (current-memory-use))
                  (t0 (current-jiffy)))
              (thunk)
              (let* ((t1 (current-jiffy))
                     (b1 (current-memory-use))
                     (wall-ns (- t1 t0))
                     (bytes (- b1 b0)))
                (loop (+ i 1)
                      (cons wall-ns wall-samples)
                      (cons bytes byte-samples)))))))))

(define (emit-result name params engine engine-tier engine-version
                     wall-samples byte-samples)
  (let* ((stats (gc-stats))
         (iters (length wall-samples))
         ; Convert ns → seconds for the wall-time fields.
         (ns->sec (lambda (n) (exact->inexact (/ n jiffies-per-second-cached))))
         (wall-sorted (list-sort < wall-samples))
         (wall-min (rw-min wall-samples))
         (wall-max (rw-max wall-samples))
         (wall-p50 (rw-percentile wall-sorted 0.5))
         (wall-p95 (rw-percentile wall-sorted 0.95))
         (wall-p99 (rw-percentile wall-sorted 0.99))
         (wall-mean (rw-mean wall-samples))
         (wall-stddev (rw-stddev wall-samples))
         (total-bytes (rw-sum byte-samples))
         (total-wall-ns (rw-sum wall-samples))
         (alloc-rate-mbps
           (if (> total-wall-ns 0)
               (/ (* total-bytes 1000.0)
                  (* (/ total-wall-ns 1000000000.0) 1048576.0))
               0.0))
         (collect-time-ms (alist-get 'collect-time-ms stats))
         (gc-time-pct
           (if (> total-wall-ns 0)
               (* 100.0 (/ (* collect-time-ms 1000000.0) total-wall-ns))
               0.0)))
    (jobj-of
      (list
        (cons "schema_version" (jstr-v "1.0"))
        (cons "timestamp" (jstr-v (rw-timestamp)))
        (cons "engine" (jstr-v engine))
        (cons "engine_tier" (jstr-v engine-tier))
        (cons "engine_version" (jstr-v engine-version))
        (cons "benchmark" (jstr-v name))
        (cons "params" (lambda () (emit-params params)))
        (cons "config" (lambda () (emit-config iters)))
        (cons "wall_time_seconds" (lambda ()
          (jobj-of
            (list
              (cons "iters" (lambda ()
                (jlist-of (map (lambda (ns) (jnum (ns->sec ns))) wall-samples))))
              (cons "min" (jnum (ns->sec wall-min)))
              (cons "p50" (jnum (ns->sec wall-p50)))
              (cons "p95" (jnum (ns->sec wall-p95)))
              (cons "p99" (jnum (ns->sec wall-p99)))
              (cons "max" (jnum (ns->sec wall-max)))
              (cons "mean" (jnum (ns->sec wall-mean)))
              (cons "stddev" (jnum (ns->sec wall-stddev)))))))
        (cons "memory" (lambda ()
          (jobj-of
            (list
              (cons "bytes_allocated_total"
                (jnum (alist-get 'bytes_allocated_total
                                 (alist-rekey stats))))
              (cons "alloc_count_total"
                (jnum (alist-get 'alloc_count_total
                                 (alist-rekey stats))))
              (cons "collect_count"
                (jnum (alist-get 'collect_count
                                 (alist-rekey stats))))
              (cons "live_slots"
                (jnum (alist-get 'live_slots
                                 (alist-rekey stats))))
              (cons "alloc_rate_mb_per_sec" (jnum alloc-rate-mbps))
              (cons "gc_time_ms" (jnum collect-time-ms))
              (cons "gc_time_pct" (jnum gc-time-pct))
              (cons "last_pause_ms"
                (jnum (alist-get 'last-pause-ms stats)))
              (cons "max_pause_ms"
                (jnum (alist-get 'max-pause-ms stats))))))) ))
    (newline)))

; jiffies-per-second is constant; cache to avoid the primop call
; in inner loops.
(define jiffies-per-second-cached (jiffies-per-second))

; The gc-stats keys use hyphens (Scheme convention); JSON output
; uses underscores. This rekeys for the JSON-side accessors.
(define (alist-rekey alist)
  (map (lambda (p)
         (cons
           (string->symbol
             (rw-replace-hyphens (symbol->string (car p))))
           (cdr p)))
       alist))

(define (rw-replace-hyphens s)
  (let* ((chars (string->list s)))
    (list->string
      (map (lambda (c) (if (char=? c #\-) #\_ c)) chars))))

(define (emit-params alist)
  (jobj-of
    (map (lambda (p)
           (cons (symbol->string (car p))
                 (cond
                   ((number? (cdr p)) (jnum (cdr p)))
                   ((string? (cdr p)) (jstr-v (cdr p)))
                   ((boolean? (cdr p)) (jbool (cdr p)))
                   (else (jstr-v (rw-to-string (cdr p)))))))
         alist)))

(define (emit-config iters)
  (jobj-of
    (list
      (cons "warmup_iters"
        (jnum (rw-env-num "REALWORLD_WARMUP_ITERS" 3)))
      (cons "max_iters"
        (jnum (rw-env-num "REALWORLD_MEASURE_ITERS" 10)))
      (cons "time_budget_seconds"
        (jnum (rw-env-num "REALWORLD_TIME_BUDGET_SEC" 60)))
      (cons "measured_iters" (jnum iters)))))

(define (rw-to-string v)
  (let* ((p (open-output-string)))
    (display v p)
    (get-output-string p)))

; rw-timestamp: ISO-ish format; we don't have a portable
; "current local time" primop, so we stamp with the
; current-second value as an integer (seconds since epoch).
; The harness wrapper can replace with a proper ISO timestamp.
(define (rw-timestamp)
  (let* ((s (current-second))
         (p (open-output-string)))
    (display "epoch:" p)
    (display s p)
    (get-output-string p)))
