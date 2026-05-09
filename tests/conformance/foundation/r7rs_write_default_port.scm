(test-section "R7RS write-char/write-string with optional port (default current-output-port)")

; --- write-char without port writes to current-output-port ---
(test-equal "wc-no-port"
  "ab"
  (with-output-to-string
    (lambda ()
      (write-char #\a)
      (write-char #\b))))

; --- write-char with explicit port still works ---
(define op1 (open-output-string))
(write-char #\X op1)
(write-char #\Y op1)
(test-equal "wc-explicit-port" "XY" (get-output-string op1))

; --- write-string without port ---
(test-equal "ws-no-port"
  "hello world"
  (with-output-to-string
    (lambda ()
      (write-string "hello ")
      (write-string "world"))))

; --- write-string with explicit port ---
(define op2 (open-output-string))
(write-string "test" op2)
(test-equal "ws-explicit-port" "test" (get-output-string op2))

; --- write-string with port + start ---
(define op3 (open-output-string))
(write-string "abcdef" op3 2)
(test-equal "ws-explicit-start" "cdef" (get-output-string op3))

; --- write-string with port + start + end ---
(define op4 (open-output-string))
(write-string "abcdef" op4 1 4)
(test-equal "ws-explicit-start-end" "bcd" (get-output-string op4))

; --- mixed: write-char and write-string interleaved (no port) ---
(test-equal "mixed-no-port"
  "Hello, World!"
  (with-output-to-string
    (lambda ()
      (write-string "Hello")
      (write-char #\,)
      (write-char #\space)
      (write-string "World")
      (write-char #\!))))

; --- write-char arity error ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (write-char))))))
(test-eqv "wc-arity-0" 'caught c1)

; --- write-char too many args ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (write-char #\a (open-output-string) 'extra))))))
(test-eqv "wc-arity-3" 'caught c2)

; --- write-string with non-character: type error ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (write-char 65))))))  ; need #\A not 65
(test-eqv "wc-non-char" 'caught c3)

; --- nested with-output-to-string ---
(test-equal "nested-output"
  "outer-inner-rest"
  (with-output-to-string
    (lambda ()
      (write-string "outer-")
      (write-string
        (with-output-to-string
          (lambda () (write-string "inner"))))
      (write-string "-rest"))))

; --- write-string with empty content ---
(test-equal "ws-empty"
  ""
  (with-output-to-string
    (lambda ()
      (write-string "")
      (write-string ""))))

; --- write-char preserves char identity ---
(test-equal "wc-many-chars"
  "0123456789"
  (with-output-to-string
    (lambda ()
      (write-char #\0)
      (write-char #\1)
      (write-char #\2)
      (write-char #\3)
      (write-char #\4)
      (write-char #\5)
      (write-char #\6)
      (write-char #\7)
      (write-char #\8)
      (write-char #\9))))
