; Binary-trees — pair allocation + GC churn.

(define (make-tree depth)
  (if (= depth 0)
      (cons #f #f)
      (cons (make-tree (- depth 1))
            (make-tree (- depth 1)))))

(define (check-tree t)
  (if (not (car t))
      1
      (+ 1 (check-tree (car t)) (check-tree (cdr t)))))

(define (run depth)
  (let loop ((d 4) (acc 0))
    (if (> d depth)
        acc
        (let* ((iters (arithmetic-shift 1 (- depth d -4)))
               (sum (let inner ((i 0) (s 0))
                      (if (>= i iters)
                          s
                          (inner (+ i 1) (+ s (check-tree (make-tree d))))))))
          (loop (+ d 2) (+ acc sum))))))

(realworld-bench
  "binary-trees"
  '((depth . 10))
  (lambda () (run 10)))
