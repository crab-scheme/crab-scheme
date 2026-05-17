; Mandelbrot — tight Flonum loops.

(define (mandelbrot-pixel cr ci)
  (let loop ((zr 0.0) (zi 0.0) (i 0))
    (cond
     ((> i 49) #t)
     ((> (+ (* zr zr) (* zi zi)) 4.0) #f)
     (else
      (loop (+ (- (* zr zr) (* zi zi)) cr)
            (+ (* 2.0 zr zi) ci)
            (+ i 1))))))

(define (mandelbrot n)
  (let row-loop ((y 0) (count 0))
    (if (= y n)
        count
        (let col-loop ((x 0) (rcount 0))
          (if (= x n)
              (row-loop (+ y 1) (+ count rcount))
              (let* ((cr (- (/ (* 2.0 x) n) 1.5))
                     (ci (- (/ (* 2.0 y) n) 1.0)))
                (col-loop (+ x 1)
                          (if (mandelbrot-pixel cr ci)
                              (+ rcount 1)
                              rcount))))))))

(realworld-bench
  "mandelbrot"
  '((n . 80))
  (lambda () (mandelbrot 80)))
