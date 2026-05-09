(test-section "R7RS time + environment + process")

; --- current-second is a flonum representing seconds since Unix epoch ---
(define s (current-second))
(test-true "current-second is flonum" (flonum? s))
; In year 2026 the value is roughly 1.7-1.8e9; assert it's positive and
; well above 2020 (1.577e9) to catch regressions where the call returned 0.
(test-true "current-second after 2020"
  (> s 1577836800.0))   ; 2020-01-01 UTC

; Two consecutive reads should be non-decreasing (allowing equality
; due to flonum rounding when the calls land in the same nanosecond).
(define s1 (current-second))
(define s2 (current-second))
(test-true "current-second monotonic" (>= s2 s1))

; --- current-jiffy is an exact non-negative integer ---
(define j (current-jiffy))
(test-true "current-jiffy is integer" (exact-integer? j))
(test-true "current-jiffy non-negative" (>= j 0))

; Two consecutive reads should be non-decreasing.
(define j1 (current-jiffy))
(define j2 (current-jiffy))
(test-true "current-jiffy monotonic" (>= j2 j1))

; --- jiffies-per-second ---
(test-eqv "jps is 10^9" 1000000000 (jiffies-per-second))

; --- get-environment-variable ---
; PATH is reliably set on POSIX systems and Windows.
(define path (get-environment-variable "PATH"))
(test-true "PATH is string or #f" (or (string? path) (not path)))
(test-equal "missing var → #f" #f
  (get-environment-variable "CRABSCHEME_DEFINITELY_UNSET_XYZZY"))

; --- get-environment-variables returns an alist ---
(define env (get-environment-variables))
(test-true "env is a list" (list? env))
(test-true "env entries are pairs"
  (or (null? env) (pair? (car env))))
; If env is non-empty, the first entry should be (string . string).
(when (not (null? env))
  (test-true "env entry car is string"
    (string? (car (car env))))
  (test-true "env entry cdr is string"
    (string? (cdr (car env)))))

; --- command-line returns a list of strings ---
(define cl (command-line))
(test-true "command-line is a list" (list? cl))
(test-true "command-line entries are strings"
  (or (null? cl) (string? (car cl))))
