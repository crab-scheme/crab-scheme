(test-section "String ports + read-char/peek-char/get-line")

; Input port
(define ip (open-string-input-port "abc"))
(test-true  "input-port?"     (input-port? ip))
(test-false "input-not-output" (output-port? ip))
(test-true  "port?-input"     (port? ip))

; read-char sequence
(test-eqv "read-char-1" #\a (read-char ip))
(test-eqv "read-char-2" #\b (read-char ip))
(test-eqv "read-char-3" #\c (read-char ip))
(test-true "read-char-eof" (eof-object? (read-char ip)))

; peek-char doesn't consume
(define ip2 (open-string-input-port "xyz"))
(test-eqv "peek-char-x" #\x (peek-char ip2))
(test-eqv "peek-then-read-x" #\x (read-char ip2))
(test-eqv "peek-after-x" #\y (peek-char ip2))

; get-line
(define ip3 (open-string-input-port "first line\nsecond line\nthird"))
(test-equal "get-line-1" "first line"  (get-line ip3))
(test-equal "get-line-2" "second line" (get-line ip3))
(test-equal "get-line-3" "third"       (get-line ip3))
(test-true  "get-line-eof" (eof-object? (get-line ip3)))

; Output port
(define op (open-string-output-port))
(test-true  "output-port?"     (output-port? op))
(test-false "output-not-input" (input-port? op))
(write-char #\h op)
(write-char #\i op)
(test-equal "output-collected" "hi" (get-output-string op))
; After get-output-string, the buffer is reset
(write-string "next" op)
(test-equal "output-after-reset" "next" (get-output-string op))

; write-string
(define op2 (open-string-output-port))
(write-string "Hello, " op2)
(write-string "world!" op2)
(test-equal "write-string-collected" "Hello, world!" (get-output-string op2))
