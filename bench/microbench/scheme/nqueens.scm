; N-queens — recursive backtracking, list / function dispatch heavy.
; Counts solutions to the 8-queens problem.
;
; (nqueens 8) = 92

(define (count-from-1-to-n proc n)
  (let loop ((i 1) (acc 0))
    (if (> i n)
        acc
        (loop (+ i 1) (+ acc (proc i))))))

(define (safe? row col placed)
  ; placed is a list of (row . col) pairs already on the board.
  (let loop ((p placed))
    (if (null? p)
        #t
        (let ((r (car (car p)))
              (c (cdr (car p))))
          (if (or (= c col)
                  (= (- r row) (- c col))
                  (= (- r row) (- col c)))
              #f
              (loop (cdr p)))))))

(define (nqueens n)
  (define (place row placed)
    (if (> row n)
        1
        (count-from-1-to-n
         (lambda (col)
           (if (safe? row col placed)
               (place (+ row 1) (cons (cons row col) placed))
               0))
         n)))
  (place 1 '()))

(display "nqueens(8) = ") (display (nqueens 8)) (newline)
