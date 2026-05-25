; Transient-cons elimination microbench (#28 / scalar-replace-cons).
;
; Each iteration builds two *directly-consumed* transient pairs —
; (car (cons i 1)) and (cdr (cons i 1)) — that never escape and are
; read back immediately. With the scalar-replace-cons optimizer pass
; (default-on for JIT) these conses are eliminated entirely: the loop
; allocates nothing per iteration. Without it, each iteration heap-
; allocates two Gc<Pair>.
;
; This is the shape that demonstrates the pass's win; it is deliberately
; allocation-bound and call-light so the cons allocation dominates. Run
; on --tier vm-jit to see SRA fire; --tier vm shows the un-eliminated
; baseline.
;
; Default n = 2_000_000. sumcc(n,0) = sum_{i=1..n}(i+1).

(define (sumcc n acc)
  (if (= n 0)
      acc
      (sumcc (- n 1)
             (+ acc (car (cons n 1)) (cdr (cons n 1))))))

(define n 2000000)
(display "sumcc(") (display n) (display ") = ") (display (sumcc n 0)) (newline)
