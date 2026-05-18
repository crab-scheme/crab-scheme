; Conformance test for `(crab time)` — stdlib-modules iter 5.

(test-section "(crab time) — wall clock")

(define __t0__ (current-time))
(define __t0ms__ (current-time-ms))
(test-true "current-time returns a positive fixnum" (> __t0__ 0))
(test-true "current-time-ms returns a positive fixnum" (> __t0ms__ 0))
(test-true "current-time-ms ≈ current-time × 1000"
           (< (abs (- __t0ms__ (* __t0__ 1000))) 2000))

(test-section "(crab time) — monotonic")

(define __m0__ (monotonic-time-ns))
(define __m1__ (monotonic-time-ns))
(test-true "monotonic-time-ns is non-negative"     (>= __m0__ 0))
(test-true "monotonic-time-ns strictly increases"  (>= __m1__ __m0__))

(test-section "(crab time) — sleep")

(define __s0__ (monotonic-time-ns))
(sleep-ms 5)
(define __s1__ (monotonic-time-ns))
; 5ms = 5,000,000ns. Tight bound: must elapse at least 1ms (timer
; coarseness on some CI hosts). Loose bound: < 500ms to catch
; runaway sleeps.
(test-true "sleep-ms 5 elapses at least 1ms"
           (> (- __s1__ __s0__) 1000000))
(test-true "sleep-ms 5 elapses less than 500ms"
           (< (- __s1__ __s0__) 500000000))

(test-section "(crab time) — format/parse")

; Unix epoch (1970-01-01T00:00:00Z) — exact strftime fixture.
(test-equal "format-time of epoch is canonical"
            "1970-01-01 00:00:00"
            (format-time 0 "%Y-%m-%d %H:%M:%S"))

; Round trip through a known value.
(define __mid__ (parse-time "2024-03-14 15:09:26" "%Y-%m-%d %H:%M:%S"))
(test-true "parse-time returns a positive fixnum" (> __mid__ 0))
(test-equal "format-time round-trip"
            "2024-03-14 15:09:26"
            (format-time __mid__ "%Y-%m-%d %H:%M:%S"))

(test-false "parse-time returns #f on garbage"
            (parse-time "not a date" "%Y-%m-%d"))
