(test-section "R6RS (endianness ...) macro and integration with bytevector ops")

; --- (endianness big) → 'big, (endianness little) → 'little ---
(test-eqv "endianness-big"    'big    (endianness big))
(test-eqv "endianness-little" 'little (endianness little))

; The expansion is a literal symbol — eq? works against quoted symbols.
(test-true "endianness eq? big" (eq? (endianness big) 'big))
(test-true "endianness eq? little" (eq? (endianness little) 'little))

; --- using the macro with bytevector typed accessors ---
(define bv (make-bytevector 8 0))
(bytevector-u32-set! bv 0 #xAABBCCDD (endianness big))
(test-eqv "u32-set via macro / get raw byte" #xAA (bytevector-u8-ref bv 0))
(test-eqv "u32 round-trip via macro" #xAABBCCDD
  (bytevector-u32-ref bv 0 (endianness big)))

(define bv2 (make-bytevector 4 0))
(bytevector-s16-set! bv2 0 -42 (endianness little))
(test-eqv "s16 -42 round-trip"
  -42 (bytevector-s16-ref bv2 0 (endianness little)))

(bytevector-ieee-double-set! bv 0 1.25 (endianness big))
(test-equal "f64 1.25 via macro" 1.25
  (bytevector-ieee-double-ref bv 0 (endianness big)))

; --- the macro expansion can pass through `let` ---
(define endian-be (endianness big))
(define endian-le (endianness little))
(test-eqv "let-bound be" 'big endian-be)
(test-eqv "let-bound le" 'little endian-le)
