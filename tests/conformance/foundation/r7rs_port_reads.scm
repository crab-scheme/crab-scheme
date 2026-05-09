(test-section "R7RS port-input ops: read-string, char-ready?, read-u8, peek-u8, u8-ready?, read-bytevector")

; --- read-string from a string-input port ---
(define (sip s) (open-input-string s))

(let ((p (sip "hello world")))
  (test-equal "rs-5"   "hello" (read-string 5 p))
  (test-equal "rs-rest" " world" (read-string 100 p))
  (test-equal "rs-eof"  (eof-object) (read-string 1 p)))

(let ((p (sip "abc")))
  (test-equal "rs-zero"  "" (read-string 0 p))
  (test-equal "rs-3"     "abc" (read-string 3 p)))

; --- read-string with multi-byte UTF-8 / wide chars ---
(let ((p (sip "héllo")))
  ; 5 chars: h é l l o (regardless of UTF-8 byte count)
  (test-equal "rs-utf8-2"  "hé"   (read-string 2 p))
  (test-equal "rs-utf8-3"  "llo"  (read-string 3 p)))

; --- char-ready? on string-input ---
(let ((p (sip "x")))
  (test-true "cr-pre"  (char-ready? p))
  (read-char p)
  ; After consuming the char, char-ready? still returns #t because
  ; read-char would return EOF without blocking.
  (test-true "cr-eof" (char-ready? p)))

; --- char-ready? on non-textual port raises ---
(test-true "cr-on-binary-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (char-ready? (open-input-bytevector #u8(1 2 3))))))

; --- read-u8 / peek-u8 on bytevector input ---
(let ((p (open-input-bytevector #u8(10 20 30))))
  (test-eqv "ru8-peek1" 10 (peek-u8 p))
  (test-eqv "ru8-peek2-still-10" 10 (peek-u8 p))
  (test-eqv "ru8-read1" 10 (read-u8 p))
  (test-eqv "ru8-read2" 20 (read-u8 p))
  (test-eqv "ru8-read3" 30 (read-u8 p))
  (test-equal "ru8-eof"  (eof-object) (read-u8 p))
  (test-equal "ru8-peek-eof" (eof-object) (peek-u8 p)))

; --- u8-ready? on a binary port ---
(let ((p (open-input-bytevector #u8(1 2))))
  (test-true "u8r-pre"  (u8-ready? p))
  (read-u8 p)
  (read-u8 p)
  (test-true "u8r-eof" (u8-ready? p)))

; --- read-bytevector reads up to k bytes ---
(let ((p (open-input-bytevector #u8(1 2 3 4 5))))
  (test-equal "rbv-3"   #u8(1 2 3) (read-bytevector 3 p))
  (test-equal "rbv-rest" #u8(4 5)   (read-bytevector 100 p))
  (test-equal "rbv-eof"  (eof-object) (read-bytevector 1 p)))

(let ((p (open-input-bytevector #u8(99))))
  (test-equal "rbv-zero" #u8() (read-bytevector 0 p))
  (test-equal "rbv-1"    #u8(99) (read-bytevector 1 p)))

; --- read-string on binary port raises ---
(test-true "rs-on-binary-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (read-string 1 (open-input-bytevector #u8(1 2))))))

; --- read-u8 on textual port raises ---
(test-true "ru8-on-textual-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (read-u8 (sip "abc")))))

; --- negative count raises ---
(test-true "rs-neg-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (read-string -1 (sip "abc")))))
(test-true "rbv-neg-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (read-bytevector -1 (open-input-bytevector #u8(1))))))
