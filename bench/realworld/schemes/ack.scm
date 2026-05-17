; ack(3, 6) — non-primitive-recursive Ackermann.

(define (ack m n)
  (cond ((= m 0) (+ n 1))
        ((= n 0) (ack (- m 1) 1))
        (else (ack (- m 1) (ack m (- n 1))))))

(realworld-bench
  "ack"
  '((m . 3) (n . 6))
  (lambda () (ack 3 6)))
