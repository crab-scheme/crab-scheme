; Mandelbrot — typed version (Phase 5 bench validation).
;
; Companion to mandelbrot.scm: identical body, plus type
; annotations on the two user-defined functions. Used by
; bench/scripts/typer-phase5-validate.sh to compare
; AOT'd binary perf with and without annotations.
;
; Default N = 100 -> 3963 pixels in the set.

(: mandelbrot-pixel (-> Flonum Flonum Boolean))
(define (mandelbrot-pixel [cr : Flonum] [ci : Flonum]) : Boolean
  (let loop ((zr 0.0) (zi 0.0) (i 0))
    (cond
     ((> i 49) #t)             ; converged: in set
     ((> (+ (* zr zr) (* zi zi)) 4.0) #f)  ; escaped
     (else
      (loop (+ (- (* zr zr) (* zi zi)) cr)
            (+ (* 2.0 zr zi) ci)
            (+ i 1))))))

(: mandelbrot (-> Fixnum Fixnum))
(define (mandelbrot [n : Fixnum]) : Fixnum
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

(define n 80)
(display "mandelbrot(") (display n) (display ") = ")
(display (mandelbrot n))
(newline)
