; Allocation-stress benchmark: builds many short-lived pairs and lists
; so the GC has to sweep frequently. Useful baseline to track ahead of
; the M5 Phase 2 arena work.
;
; Each "round" builds a 1000-element list, computes its length, and
; throws it away. Total: 200 rounds × 1000 pairs = 200,000 pairs
; allocated. The result is the sum-of-lengths, which checks all
; rounds completed.
;
; (alloc-stress 200) = 200000

(define (make-list-n n)
  (let loop ((i 0) (acc '()))
    (if (= i n)
        acc
        (loop (+ i 1) (cons i acc)))))

(define (alloc-stress rounds)
  (let loop ((r 0) (sum 0))
    (if (= r rounds)
        sum
        (loop (+ r 1)
              (+ sum (length (make-list-n 1000)))))))

(define n 200)
(display "alloc-stress(") (display n) (display ") = ")
(display (alloc-stress n))
(newline)
