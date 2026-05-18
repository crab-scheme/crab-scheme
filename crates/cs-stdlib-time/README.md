# `(crab time)` — Wall clock, monotonic, sleep, strftime

CrabScheme stdlib module wrapping `std::time` + `chrono` for
formatting. Iter 5 of the stdlib-modules spec.

Time values are passed as fixnums (epoch seconds for wall,
nanoseconds since process start for monotonic) until the FFI gains
an opaque-payload Scheme value for typed Instant / Duration.

## Procedures

```
(current-time)              ;-> fixnum   ; Unix epoch seconds
(current-time-ms)           ;-> fixnum   ; Unix epoch milliseconds
(monotonic-time-ns)         ;-> fixnum   ; ns since process start; strictly increases
(sleep-ms ms)               ;-> unspec   ; block the current thread

(format-time epoch-secs fmt)  ;-> string  ; strftime; epoch-secs in UTC
(parse-time str fmt)          ;-> fixnum or #f  ; strptime; returns epoch seconds
```

## Example

```scheme
(import (crab time))

(display "now is ") (display (format-time (current-time) "%Y-%m-%d %H:%M:%S")) (newline)

;; benchmark a thunk in milliseconds
(define (bench thunk)
  (let ((t0 (monotonic-time-ns)))
    (thunk)
    (/ (- (monotonic-time-ns) t0) 1000000.0)))

(display "elapsed: ") (display (bench (lambda () (sleep-ms 20)))) (display "ms") (newline)
```

## Notes

- All formatted/parsed timestamps are UTC. Local-time support and a
  proper `time-zone` value land in a follow-up iter.
- `sleep-ms` blocks the calling thread; the runtime is
  single-threaded so it blocks all Scheme work. Use the BEAM actor
  primops if you need to wait without blocking other concurrent
  Scheme tasks.
