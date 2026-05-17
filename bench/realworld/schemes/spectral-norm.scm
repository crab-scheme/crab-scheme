; Spectral norm — Flonum + vector access.

(define (matrix-elt i j)
  (let* ((ij (+ i j))
         (denom (+ (/ (* ij (+ ij 1)) 2) (+ i 1))))
    (/ 1.0 denom)))

(define (mul-Av n v out)
  (let i-loop ((i 0))
    (if (< i n)
        (begin
          (let j-loop ((j 0) (sum 0.0))
            (if (< j n)
                (j-loop (+ j 1) (+ sum (* (matrix-elt i j) (vector-ref v j))))
                (vector-set! out i sum)))
          (i-loop (+ i 1)))
        out)))

(define (mul-Atv n v out)
  (let i-loop ((i 0))
    (if (< i n)
        (begin
          (let j-loop ((j 0) (sum 0.0))
            (if (< j n)
                (j-loop (+ j 1) (+ sum (* (matrix-elt j i) (vector-ref v j))))
                (vector-set! out i sum)))
          (i-loop (+ i 1)))
        out)))

(define (mul-AtAv n v out tmp)
  (mul-Av n v tmp)
  (mul-Atv n tmp out))

(define (spectral-norm n)
  (let ((u (make-vector n 1.0))
        (v (make-vector n 0.0))
        (tmp (make-vector n 0.0)))
    (let iter ((k 0))
      (if (< k 10)
          (begin
            (mul-AtAv n u v tmp)
            (mul-AtAv n v u tmp)
            (iter (+ k 1)))
          'done))
    (let dot-loop ((i 0) (vBv 0.0) (vv 0.0))
      (if (= i n)
          (sqrt (/ vBv vv))
          (dot-loop (+ i 1)
                    (+ vBv (* (vector-ref u i) (vector-ref v i)))
                    (+ vv (* (vector-ref v i) (vector-ref v i))))))))

(realworld-bench
  "spectral-norm"
  '((n . 50))
  (lambda () (spectral-norm 50)))
