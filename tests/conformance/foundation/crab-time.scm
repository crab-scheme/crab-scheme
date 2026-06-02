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

(test-section "(crab time) — date components")
; Build a known instant so the test is self-consistent regardless of tz.
(define __dt__ (time-make 2023 11 14 22 13 20))  ; 2023-11-14 22:13:20 UTC
(test-equal "time-make matches the known epoch" 1700000000 __dt__)
(test-equal "time-year" 2023 (time-year __dt__))
(test-equal "time-month" 11 (time-month __dt__))
(test-equal "time-day" 14 (time-day __dt__))
(test-equal "time-hour" 22 (time-hour __dt__))
(test-equal "time-minute" 13 (time-minute __dt__))
(test-equal "time-second" 20 (time-second __dt__))
(test-equal "time-weekday (2023-11-14 is Tuesday = 2)" 2 (time-weekday __dt__))

(test-section "(crab time) — date arithmetic")
(test-equal "time-add-days advances one day" 1700086400 (time-add-days __dt__ 1))
(test-equal "time-add-days goes backward" 1699913600 (time-add-days __dt__ -1))
(test-equal "make then extract round-trips the day" 25 (time-day (time-make 2020 12 25 0 0 0)))

(test-section "(crab time) — calendar")
(test-true "2000 is a leap year (divisible by 400)" (time-leap-year? 2000))
(test-false "1900 is not a leap year (divisible by 100, not 400)" (time-leap-year? 1900))
(test-true "2024 is a leap year" (time-leap-year? 2024))
(test-false "2023 is not a leap year" (time-leap-year? 2023))
(test-equal "February 2024 has 29 days" 29 (time-days-in-month 2024 2))
(test-equal "February 2023 has 28 days" 28 (time-days-in-month 2023 2))
(test-equal "April has 30 days" 30 (time-days-in-month 2023 4))
(test-equal "January has 31 days" 31 (time-days-in-month 2023 1))
(test-equal "day-of-year of Jan 1" 1 (time-day-of-year (time-make 2023 1 1 0 0 0)))
(test-equal "day-of-year of Dec 31 (non-leap)" 365 (time-day-of-year (time-make 2023 12 31 0 0 0)))
