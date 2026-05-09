(test-section "string->utf8 / utf8->string + bytevector-append/-fill!")

; ASCII roundtrip
(test-equal "ascii-roundtrip" "hello"
  (utf8->string (string->utf8 "hello")))

; Empty string
(test-equal "empty-roundtrip" ""
  (utf8->string (string->utf8 "")))
(test-eqv "empty-byte-len" 0
  (bytevector-length (string->utf8 "")))

; Non-ASCII (é = 0xC3 0xA9)
(test-equal "non-ascii-roundtrip" "héllo"
  (utf8->string (string->utf8 "héllo")))
(test-equal "non-ascii-bytes" '(104 195 169 108 108 111)
  (bytevector->u8-list (string->utf8 "héllo")))

; Multi-byte char counts as one Scheme char
(test-eqv "non-ascii-string-length" 5 (string-length "héllo"))

; Byte length differs from char length for non-ASCII
(test-eqv "non-ascii-byte-length" 6
  (bytevector-length (string->utf8 "héllo")))

; Range encoding ([start, end) over CHARACTER indices)
(test-equal "range-sub" "bcd"
  (utf8->string (string->utf8 "abcdef" 1 4)))

; Range decoding ([start, end) over BYTE indices)
(test-equal "decode-range" "ello"
  (utf8->string (string->utf8 "hello") 1 5))

; bytevector-append concatenates
(test-equal "bv-append-strings" "abcdef"
  (utf8->string
    (bytevector-append (string->utf8 "ab")
                       (string->utf8 "cd")
                       (string->utf8 "ef"))))

; Empty append yields empty
(test-eqv "bv-append-empty" 0
  (bytevector-length (bytevector-append)))

; bytevector-fill! mutates in place
(define bv (make-bytevector 4 0))
(bytevector-fill! bv 99)
(test-equal "bv-fill-result" '(99 99 99 99) (bytevector->u8-list bv))

; Invalid UTF-8 raises a catchable condition
(test-true "invalid-utf8-raises"
  (with-exception-handler
    (lambda (c) (and (error? c)
                     (eq? (condition-who c) 'utf8->string)))
    (lambda () (utf8->string (bytevector 195 40)))))

; Type errors on string->utf8
(test-true "string->utf8-type-error"
  (with-exception-handler
    (lambda (c) (and (error? c)
                     (eq? (condition-who c) 'string->utf8)))
    (lambda () (string->utf8 42))))
