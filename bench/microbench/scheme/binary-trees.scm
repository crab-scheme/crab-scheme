; Binary-trees — Computer Language Benchmarks Game.
; Allocates and traverses many short-lived binary trees. The principal
; cost is pair allocation and GC; therefore a good GC stress test.
;
; Default depth = 12. Output is a single integer (the sum of leaf checks).

(define (make-tree depth)
  (if (= depth 0)
      (cons #f #f)
      (cons (make-tree (- depth 1))
            (make-tree (- depth 1)))))

(define (check-tree t)
  ; Return 1 for a leaf, otherwise 1 + count of both children.
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

(define depth 10)
(display "binary-trees(") (display depth) (display ") = ")
(display (run depth))
(newline)
