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

; ---- R6RS §8.2 — put-char / put-string / put-bytevector --------------
(test-section "R6RS port writes")

(let ((p (open-string-output-port)))
  (put-char p #\h)
  (put-char p #\i)
  (test-equal "put-char-builds-string" "hi" (get-output-string p)))

(let ((p (open-string-output-port)))
  (put-string p "hello")
  (put-string p " world!" 0 6)
  (test-equal "put-string-with-slice" "hello world" (get-output-string p)))

(let ((p (open-string-output-port)))
  (put-string p "abcdef" 2 3)
  (test-equal "put-string-mid-slice" "cde" (get-output-string p)))

(let ((p (open-output-bytevector)))
  (put-bytevector p (bytevector 1 2 3 4 5))
  (put-bytevector p (bytevector 10 20 30 40) 1 2)
  (test-equal "put-bytevector-with-slice"
              #u8(1 2 3 4 5 20 30)
              (get-output-bytevector p)))

; ---- R6RS get-bytevector-all / get-string-n -------------------------
(test-section "R6RS port reads")

(let ((p (open-bytevector-input-port (bytevector 1 2 3 4 5))))
  (test-equal "gba-takes-some"  #u8(1 2)   (get-bytevector-n p 2))
  (test-equal "gba-takes-rest"  #u8(3 4 5) (get-bytevector-all p))
  (test-true  "gba-eof-after"   (eof-object? (get-bytevector-all p))))

(let ((p (open-string-input-port "Hello, world!")))
  (test-equal "gsn-takes-some"  "Hello" (get-string-n p 5))
  (test-equal "gsn-takes-rest"  ", world!" (get-string-n p 1000))
  (test-true  "gsn-eof-after"   (eof-object? (get-string-n p 1))))

(let ((p (open-string-input-port "")))
  (test-true  "gsn-eof-empty"   (eof-object? (get-string-n p 5))))

; ---- R6RS standard-{input,output,error}-port -----------------------
(test-section "R6RS standard ports exist")

; The exact return value depends on how the runtime is hooked up
; (REPL/file/embedded). For now we only verify the procedures exist
; and don't error.
(test-true "standard-input-exists"  (procedure? standard-input-port))
(test-true "standard-output-exists" (procedure? standard-output-port))
(test-true "standard-error-exists"  (procedure? standard-error-port))
(test-true "standard-error-is-port" (port? (standard-error-port)))
