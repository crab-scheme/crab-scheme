; Conformance test for `(crab metrics)` — stdlib-modules iter 8.
;
; The registry is process-global; reset at the start so a previous
; test run doesn't pollute counts.

(metrics-reset!)

(test-section "(crab metrics) — counter")

(test-eqv "missing counter reads as 0"  0 (counter-value "requests"))
(counter-increment! "requests")
(counter-increment! "requests")
(counter-increment! "requests" 5)
(test-eqv "counter accumulates" 7 (counter-value "requests"))

(test-section "(crab metrics) — gauge")

(gauge-set! "queue-depth" 42)
(test-eqv "gauge round-trip" 42 (gauge-value "queue-depth"))
(gauge-set! "queue-depth" 17)
(test-eqv "gauge overwrites"  17 (gauge-value "queue-depth"))

(test-section "(crab metrics) — histogram")

(for-each (lambda (v) (histogram-observe! "latency-ms" v))
          '(1 2 3 4 5 6 7 8 9 10))

(define __h__ (histogram-summary "latency-ms"))
(test-eqv "histogram count"  10  (cdr (assoc "count" __h__)))
(test-true "histogram min ≤ p50 ≤ max"
           (let ((min (cdr (assoc "min" __h__)))
                 (p50 (cdr (assoc "p50" __h__)))
                 (max (cdr (assoc "max" __h__))))
             (and (<= min p50) (<= p50 max))))

(test-section "(crab metrics) — type clash")

(test-true "incrementing a gauge raises"
           (guard (e (#t #t))
             (counter-increment! "queue-depth")
             #f))

(test-section "(crab metrics) — snapshot")

(define __snap__ (metrics-snapshot))
(test-true "snapshot contains counter"
           (member '("requests" . "counter") __snap__))
(test-true "snapshot contains gauge"
           (member '("queue-depth" . "gauge") __snap__))
(test-true "snapshot contains histogram"
           (member '("latency-ms" . "histogram") __snap__))

(test-section "(crab metrics) — reset")

(metrics-reset!)
(test-eqv "after reset counter is 0" 0 (counter-value "requests"))
(test-equal "after reset snapshot empty" '() (metrics-snapshot))
