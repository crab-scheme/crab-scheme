; Conformance test for `(crab tty)` — stdlib-modules iter 13.
;
; Under `cargo test`, stdin/stdout/stderr are pipes, not ttys.
; The isatty predicates must all return #f in that environment.
; `terminal-size` may return #f or a list depending on the test
; runner — only the shape is asserted.

(test-section "(crab tty) — isatty under cargo test")

(test-false "stdin is not a tty under cargo test" (tty-stdin?))
(test-false "stdout is not a tty under cargo test" (tty-stdout?))
(test-false "stderr is not a tty under cargo test" (tty-stderr?))

(test-section "(crab tty) — terminal-size shape")

(define __s__ (terminal-size))
(test-true "terminal-size returns either #f or (cols rows)"
           (cond ((not __s__) #t)
                 ((and (list? __s__)
                       (= (length __s__) 2)
                       (number? (car __s__))
                       (number? (cadr __s__)))
                  #t)
                 (else #f)))
