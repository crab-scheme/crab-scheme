(test-section "R7RS string-copy / bytevector-copy with start/end")

; --- string-copy: full copy (R6RS-compatible 1-arg form) ---
(test-equal "scopy-full" "hello" (string-copy "hello"))

; --- string-copy is a fresh allocation ---
(define src "abc")
(define dst (string-copy src))
(string-set! dst 0 #\X)
(test-equal "scopy-independent-orig" "abc" src)
(test-equal "scopy-independent-copy" "Xbc" dst)

; --- string-copy with start ---
(test-equal "scopy-start" "lo world" (string-copy "hello world" 3))

; --- string-copy with start + end ---
(test-equal "scopy-start-end" "ell" (string-copy "hello" 1 4))

; --- string-copy empty slice ---
(test-equal "scopy-empty" "" (string-copy "abc" 1 1))
(test-equal "scopy-empty-zero" "" (string-copy "abc" 0 0))
(test-equal "scopy-empty-end" "" (string-copy "abc" 3 3))

; --- string-copy with multibyte UTF-8 ---
; "αβγδε" has 5 chars
(test-equal "scopy-utf8-full" "αβγδε" (string-copy "αβγδε"))
(test-equal "scopy-utf8-mid" "βγ" (string-copy "αβγδε" 1 3))
(test-equal "scopy-utf8-suffix" "δε" (string-copy "αβγδε" 3))

; --- string-copy out-of-range ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string-copy "abc" 5))))))
(test-eqv "scopy-bad-start" 'caught c1)

(test-section "bytevector-copy")

; --- bytevector-copy: full copy (R6RS-compatible 1-arg form) ---
(test-equal "bvcopy-full" #u8(1 2 3 4) (bytevector-copy #u8(1 2 3 4)))

; --- bytevector-copy is a fresh allocation ---
(define bvsrc #u8(10 20 30))
(define bvdst (bytevector-copy bvsrc))
(bytevector-u8-set! bvdst 0 99)
(test-equal "bvcopy-independent-orig" #u8(10 20 30) bvsrc)
(test-equal "bvcopy-independent-copy" #u8(99 20 30) bvdst)

; --- bytevector-copy with start ---
(test-equal "bvcopy-start" #u8(3 4 5) (bytevector-copy #u8(1 2 3 4 5) 2))

; --- bytevector-copy with start + end ---
(test-equal "bvcopy-start-end" #u8(2 3 4) (bytevector-copy #u8(1 2 3 4 5) 1 4))

; --- bytevector-copy empty slice ---
(test-equal "bvcopy-empty"      #u8() (bytevector-copy #u8(1 2 3) 1 1))
(test-equal "bvcopy-empty-zero" #u8() (bytevector-copy #u8(1 2 3) 0 0))

; --- bytevector-copy out-of-range start ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (bytevector-copy #u8(1 2 3) 99))))))
(test-eqv "bvcopy-bad-start" 'caught c2)

; --- bytevector-copy with end < start ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (bytevector-copy #u8(1 2 3) 2 1))))))
(test-eqv "bvcopy-end-before-start" 'caught c3)

; --- bytevector-copy boundary indices ---
(test-equal "bvcopy-len-len" #u8() (bytevector-copy #u8(1 2 3) 3 3))
(test-equal "bvcopy-zero-len" #u8(1 2 3) (bytevector-copy #u8(1 2 3) 0 3))
