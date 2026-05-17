; Allocation stress — 200k short-lived pair allocs/run.

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

(realworld-bench
  "alloc-stress"
  '((rounds . 200))
  (lambda () (alloc-stress 200)))
