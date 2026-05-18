# `(crab metrics)` — In-process counters, gauges, histograms

CrabScheme stdlib module. Iter 8 of the stdlib-modules spec.
Self-contained (no `metrics` ecosystem crate dep); shape mirrors
Prometheus / OpenTelemetry vocabulary so a future bridge can
shovel `(metrics-snapshot)` output into either system.

## Procedures

```
(counter-increment! name [delta=1])  ;-> unspec   ; lazy-create
(counter-value name)                 ;-> fixnum   ; 0 if missing

(gauge-set! name value)              ;-> unspec
(gauge-value name)                   ;-> fixnum

(histogram-observe! name value)      ;-> unspec
(histogram-summary name)             ;-> alist of (("count" . N) ("min" . v) ("p50" . v) ("p95" . v) ("p99" . v) ("max" . v) ("sum" . v))

(metrics-snapshot)                   ;-> alist of (("name" . kind))
(metrics-reset!)                     ;-> unspec   ; drop entire registry
```

Type clashes raise: incrementing a name registered as a gauge
(or vice versa) errors. `metrics-reset!` is primarily for tests.

## Example

```scheme
(import (crab metrics))
(import (crab time))

(define (handle-request)
  (counter-increment! "requests")
  (let ((t0 (monotonic-time-ns)))
    (do-work)
    (histogram-observe! "latency-ms"
                        (/ (- (monotonic-time-ns) t0) 1000000.0))))

;; periodically:
(let ((h (histogram-summary "latency-ms")))
  (log-info "p50=" (cdr (assoc "p50" h))
            "p99=" (cdr (assoc "p99" h))
            "count=" (cdr (assoc "count" h))))
```

## Caveats

- Histograms store every observation into a `Vec<f64>` for now.
  Long-running processes that observe hot paths will accumulate
  memory unbounded — call `metrics-reset!` periodically, or watch
  for the future sparse-bucket implementation.
- The registry is process-global, mutex-protected. Concurrent
  Scheme tasks (BEAM actors) see the same set; concurrent
  `counter-increment!` calls serialize on the registry lock.
