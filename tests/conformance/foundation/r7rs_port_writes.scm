(test-section "R7RS port-output ops: write-string slicing, write-u8, write-bytevector")

; --- write-string default form (whole string) ---
(define p1 (open-output-string))
(write-string "hello" p1)
(test-equal "ws-whole" "hello" (get-output-string p1))

; --- write-string with start index only ---
(define p2 (open-output-string))
(write-string "abcdef" p2 2)
(test-equal "ws-start-only" "cdef" (get-output-string p2))

; --- write-string with start + end ---
(define p3 (open-output-string))
(write-string "abcdefg" p3 1 4)
(test-equal "ws-start-end" "bcd" (get-output-string p3))

; --- write-string empty slice (start = end) ---
(define p4 (open-output-string))
(write-string "abcdef" p4 3 3)
(test-equal "ws-empty-slice" "" (get-output-string p4))

; --- write-string with multibyte UTF-8 ---
(define p5 (open-output-string))
(write-string "αβγδ" p5 1 3)  ; chars β γ
(test-equal "ws-utf8-slice" "βγ" (get-output-string p5))

; --- write-string concat across multiple calls ---
(define p6 (open-output-string))
(write-string "Hello, " p6)
(write-string "World!" p6)
(test-equal "ws-concat" "Hello, World!" (get-output-string p6))

; --- write-string after slicing then full ---
(define p7 (open-output-string))
(write-string "abcdef" p7 0 3)
(write-string "XYZ" p7)
(test-equal "ws-slice-then-full" "abcXYZ" (get-output-string p7))

; --- write-u8 single byte ---
(define b1 (open-output-bytevector))
(write-u8 65 b1)
(write-u8 66 b1)
(write-u8 67 b1)
(test-equal "wu8-bytes" #u8(65 66 67) (get-output-bytevector b1))

; --- write-u8 boundary values 0 and 255 ---
(define b2 (open-output-bytevector))
(write-u8 0 b2)
(write-u8 255 b2)
(test-equal "wu8-boundary" #u8(0 255) (get-output-bytevector b2))

; --- write-bytevector full bv ---
(define b3 (open-output-bytevector))
(write-bytevector #u8(1 2 3 4) b3)
(test-equal "wbv-full" #u8(1 2 3 4) (get-output-bytevector b3))

; --- write-bytevector with start index ---
(define b4 (open-output-bytevector))
(write-bytevector #u8(10 20 30 40 50) b4 2)
(test-equal "wbv-start" #u8(30 40 50) (get-output-bytevector b4))

; --- write-bytevector with start and end ---
(define b5 (open-output-bytevector))
(write-bytevector #u8(10 20 30 40 50) b5 1 4)
(test-equal "wbv-slice" #u8(20 30 40) (get-output-bytevector b5))

; --- write-bytevector empty slice ---
(define b6 (open-output-bytevector))
(write-bytevector #u8(1 2 3) b6 1 1)
(test-equal "wbv-empty-slice" #u8() (get-output-bytevector b6))

; --- mixed writes (byte + bytevector) ---
(define b7 (open-output-bytevector))
(write-u8 100 b7)
(write-bytevector #u8(101 102) b7)
(write-u8 103 b7)
(test-equal "wbv-mixed" #u8(100 101 102 103) (get-output-bytevector b7))

; --- R7RS aliases match R6RS originals ---
(test-true "open-output-string-alias"
  (output-port? (open-output-string)))
(test-true "open-output-bytevector-alias"
  (output-port? (open-output-bytevector)))
