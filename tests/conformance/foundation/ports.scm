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

; ---- R6RS §8.2.5 — codecs / transcoders -----------------------------
(test-section "R6RS transcoders")

(test-true "utf-8-codec-callable"  (procedure? utf-8-codec))
(test-true "latin-1-codec-callable" (procedure? latin-1-codec))
(test-true "utf-16-codec-callable" (procedure? utf-16-codec))
(test-true "make-transcoder-1arg"  (vector? (make-transcoder (utf-8-codec))))
(test-equal "native-eol-style-lf"  'lf (native-eol-style))

(let ((t (native-transcoder)))
  (test-true  "native-transcoder-vec"  (vector? t))
  (test-equal "native-eol-style-from-t" 'lf (transcoder-eol-style t))
  (test-equal "native-error-mode"      'replace
              (transcoder-error-handling-mode t)))

; --- bytevector<->string round-trip via UTF-8 ---
(let* ((s "Hello, 世界!")
       (t (native-transcoder))
       (bv (string->bytevector s t))
       (back (bytevector->string bv t)))
  (test-equal "utf-8-roundtrip" s back))

; --- bytevector<->string round-trip via Latin-1 ---
(let* ((s "café")
       (t (make-transcoder (latin-1-codec)))
       (bv (string->bytevector s t))
       (back (bytevector->string bv t)))
  (test-equal "latin-1-roundtrip" s back)
  (test-equal "latin-1-byte-length" 4 (bytevector-length bv)))

; --- Latin-1 rejects code points >= 256 ---
(test-true "latin-1-rejects-bmp"
  (guard (c (#t #t))
    (string->bytevector "世" (make-transcoder (latin-1-codec)))
    #f))

; --- transcoded-port wraps a binary input port for textual reads ---
(let* ((bv (string->bytevector "abc" (native-transcoder)))
       (binp (open-bytevector-input-port bv))
       (txp (transcoded-port binp (native-transcoder))))
  (test-true  "txp-textual"   (textual-port? txp))
  (test-equal "txp-content"   "abc" (get-string-all txp)))

; ---- R6RS §8.2.6 — port positions -----------------------------------
(test-section "R6RS port positions")

(let ((p (open-string-input-port "Hello")))
  (test-equal "pos-initial"   0 (port-position p))
  (read-char p) (read-char p)
  (test-equal "pos-after-2"   2 (port-position p))
  (set-port-position! p 4)
  (test-equal "pos-set"       4 (port-position p))
  (test-equal "char-after-seek" #\o (read-char p)))

(let ((p (open-string-output-port)))
  (test-equal "out-pos-initial" 0 (port-position p))
  (put-string p "abc")
  (test-equal "out-pos-after"   3 (port-position p)))

(let ((p (open-bytevector-input-port (bytevector 1 2 3 4 5))))
  (test-equal "bv-pos-initial" 0 (port-position p))
  (get-bytevector-n p 3)
  (test-equal "bv-pos-after"   3 (port-position p))
  (set-port-position! p 1)
  (test-equal "bv-after-seek"  #u8(2 3 4) (get-bytevector-n p 3)))

(test-true  "input-has-set-pos"
  (port-has-set-port-position!? (open-string-input-port "x")))
(test-false "output-no-set-pos"
  (port-has-set-port-position!? (open-string-output-port)))
(test-true  "any-port-has-pos"
  (port-has-port-position? (open-string-output-port)))

; lookahead-char alias
(let ((p (open-string-input-port "ab")))
  (test-equal "lookahead-char-1" #\a (lookahead-char p))
  (test-equal "lookahead-no-advance" #\a (lookahead-char p))
  (read-char p)
  (test-equal "lookahead-after-read" #\b (lookahead-char p)))

; out-of-range set-port-position! errors
(test-true "set-pos-rejects-overflow"
  (guard (c (#t #t))
    (let ((p (open-string-input-port "xy")))
      (set-port-position! p 5)
      #f)))
