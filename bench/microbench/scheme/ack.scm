; Ackermann — non-primitive-recursive growth, deep call stacks.
; Tests function-call dispatch with no tail-call optimization possible.
;
; ack(3, 6) = 509. Walker tier runs on host stack so deeper N is
; possible only on the VM tier. Keep N matched across tiers.

(define (ack m n)
  (cond ((= m 0) (+ n 1))
        ((= n 0) (ack (- m 1) 1))
        (else (ack (- m 1) (ack m (- n 1))))))

(display "ack(3, 6) = ") (display (ack 3 6)) (newline)
