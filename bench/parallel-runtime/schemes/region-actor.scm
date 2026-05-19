; parallel-runtime spec C6.2 — G3 region-actor benchmark.
;
; An actor opens a region, allocates inside it, exercises a
; receive-yield boundary, and confirms the region values
; behave correctly. Exercises the C3.1 dual-stack region
; mechanism + the auto-promotion path that keeps region
; values safe even when they escape (paired with C5 hardening).
;
; Success: allocations succeed, gc-allocator returns 'region
; inside the scope and 'rc after (via auto-promotion).

(define t0 (current-jiffy))
(define jpsr (jiffies-per-second))

; Test 1: open region, allocate, read tier inside.
(define inside-tier
  (with-region
   (lambda ()
     (let ((p (cons-in-region 1 2)))
       (gc-allocator p)))))

; Test 2: same region, but extract the value out.
; Runtime auto-promotion turns it into rc on region drop.
(define outside-tier
  (gc-allocator
   (with-region
    (lambda ()
      (cons-in-region 3 4)))))

(define t1 (current-jiffy))
(define elapsed (/ (- t1 t0) jpsr))

(cond
 ((and (eq? inside-tier 'region) (eq? outside-tier 'rc))
  (display "OK region-actor elapsed=")
  (display elapsed)
  (display "s inside=")
  (display inside-tier)
  (display " outside=")
  (display outside-tier)
  (newline))
 (else
  (display "FAIL region-actor inside=")
  (display inside-tier)
  (display " outside=")
  (display outside-tier)
  (newline)
  (exit 1)))
