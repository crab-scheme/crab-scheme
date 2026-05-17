; Phase B of the real-world bench suite spec — Scheme-facing GC
; instrumentation primops.
;
; Validates each primop's shape and behavior. Runs identically on
; walker, VM, and AOT tiers (jit_conformance-style three-tier
; cross-check).

(test-section "(gc-stats) returns an alist with all documented keys")

(define stats (gc-stats))
(test-true "gc-stats returns a list" (pair? stats))

; Helper: lookup an alist key, return #f if missing.
(define (alist-get key alist)
  (let loop ((rest alist))
    (cond ((null? rest) #f)
          ((eq? (car (car rest)) key) (cdr (car rest)))
          (else (loop (cdr rest))))))

(test-true "has bytes-allocated-total"
           (number? (alist-get 'bytes-allocated-total stats)))
(test-true "has alloc-count-total"
           (number? (alist-get 'alloc-count-total stats)))
(test-true "has collect-count"
           (number? (alist-get 'collect-count stats)))
(test-true "has live-slots"
           (number? (alist-get 'live-slots stats)))
(test-true "has collect-time-ms"
           (number? (alist-get 'collect-time-ms stats)))
(test-true "has last-pause-ms"
           (number? (alist-get 'last-pause-ms stats)))
(test-true "has max-pause-ms"
           (number? (alist-get 'max-pause-ms stats)))
(test-true "has stats-enabled?"
           (boolean? (alist-get 'stats-enabled? stats)))

(test-section "(gc-stats-reset!) zeroes counters")

; Force some allocation + a collect to push counters above zero,
; then verify reset wipes them.
(define junk (make-vector 1000 0))
(collect-garbage)
(define before (gc-stats))
(test-true "alloc-count-total > 0 before reset"
           (> (alist-get 'alloc-count-total before) 0))

(gc-stats-reset!)
(define after-reset (gc-stats))
(test-eqv "bytes-allocated-total = 0 after reset"
          0 (alist-get 'bytes-allocated-total after-reset))
(test-eqv "alloc-count-total = 0 after reset"
          0 (alist-get 'alloc-count-total after-reset))
(test-eqv "collect-count = 0 after reset"
          0 (alist-get 'collect-count after-reset))
(test-eqv "last-pause-ms = 0 after reset"
          0.0 (alist-get 'last-pause-ms after-reset))
(test-eqv "max-pause-ms = 0 after reset"
          0.0 (alist-get 'max-pause-ms after-reset))

(test-section "(gc-stats-enable!) / (gc-stats-disable!) flip the flag")

(gc-stats-disable!)
(test-false "stats-enabled? is #f after disable"
            (alist-get 'stats-enabled? (gc-stats)))

(gc-stats-enable!)
(test-true "stats-enabled? is #t after enable"
           (alist-get 'stats-enabled? (gc-stats)))

(test-section "(collect-garbage) forces a collection")

(define before-count (alist-get 'collect-count (gc-stats)))
(collect-garbage)
(define after-count (alist-get 'collect-count (gc-stats)))
(test-eqv "collect-count increments by 1"
          (+ before-count 1) after-count)

(test-section "(collect-garbage) records pause time when stats enabled")

(gc-stats-enable!)
(gc-stats-reset!)
; Allocate enough so the sweep does observable work.
(define heap-warmup (make-vector 5000 'x))
(collect-garbage)
(test-true "last-pause-ms > 0 with stats on"
           (> (alist-get 'last-pause-ms (gc-stats)) 0.0))
(test-true "collect-time-ms > 0 with stats on"
           (> (alist-get 'collect-time-ms (gc-stats)) 0.0))

(test-section "(current-memory-use) reflects allocations")

(gc-stats-reset!)
(define mem0 (current-memory-use))
(define alloc-junk (make-vector 1000 0))
(define mem1 (current-memory-use))
(test-true "current-memory-use grows after allocation"
           (> mem1 mem0))

(test-section "primops compose into a time-apply-style harness")

; time-apply is intentionally NOT a builtin -- the VM dispatches
; higher-order builtins through marker downcasts and adding one
; for time-apply is out of scope for Phase B. The bench harness
; builds it from the primitives, which all work on all tiers.

(define (timed-apply thunk args)
  (let ((bytes-before (current-memory-use))
        (real-start (current-jiffy)))
    (let ((result (apply thunk args)))
      (let ((real-elapsed-ns (- (current-jiffy) real-start))
            (bytes-delta (- (current-memory-use) bytes-before)))
        (list result
              (/ real-elapsed-ns 1000000.0)  ; real-ms
              bytes-delta)))))               ; bytes

(define (square x) (* x x))
(define t1 (timed-apply square '(7)))
(test-eqv "timed-apply forwards single arg" 49 (car t1))
(test-true "timed-apply returns flonum real-ms" (number? (cadr t1)))
(test-true "timed-apply real-ms >= 0" (>= (cadr t1) 0.0))
(test-true "timed-apply bytes is integer" (integer? (caddr t1)))

(define (add3 a b c) (+ a b c))
(define t2 (timed-apply add3 '(10 20 30)))
(test-eqv "timed-apply forwards multiple args" 60 (car t2))

(define (alloc-some)
  ; Allocate ~1000 pairs so the byte delta is reliably > 0.
  (let loop ((i 0) (acc '()))
    (if (= i 1000) acc (loop (+ i 1) (cons i acc)))))

(define t3 (timed-apply alloc-some '()))
(test-true "timed-apply reports bytes > 0 for an allocating thunk"
           (> (caddr t3) 0))
