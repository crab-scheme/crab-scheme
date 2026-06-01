; Conformance test for `(crab binary)` — struct-style pack/unpack.

(test-section "(crab binary) — round-trip")
(test-equal "binary-size of >ihb" 7 (binary-size ">ihb"))
(define __p__ (binary-pack ">ihb" 1000000 -5 65))
(test-true "pack returns a bytevector" (bytevector? __p__))
(test-equal "packed length" 7 (bytevector-length __p__))
(test-equal "round-trips in format order" '(1000000 -5 65) (binary-unpack ">ihb" __p__))

(test-section "(crab binary) — endianness")
(test-equal "big-endian u16 byte order" '(1 2)
            (let ((bv (binary-pack ">H" 258)))
              (list (bytevector-u8-ref bv 0) (bytevector-u8-ref bv 1))))
(test-equal "little-endian u16 byte order" '(2 1)
            (let ((bv (binary-pack "<H" 258)))
              (list (bytevector-u8-ref bv 0) (bytevector-u8-ref bv 1))))
(test-equal "little-endian round-trips" '(258) (binary-unpack "<H" (binary-pack "<H" 258)))

(test-section "(crab binary) — floats + signedness")
(test-equal "f64 round-trips" '(3.5) (binary-unpack ">d" (binary-pack ">d" 3.5)))
(test-equal "f32 round-trips" '(1.5) (binary-unpack ">f" (binary-pack ">f" 1.5)))
(test-equal "signed 8-bit negative" '(-1) (binary-unpack ">b" (binary-pack ">b" -1)))
(test-equal "unsigned 8-bit max" '(255) (binary-unpack ">B" (binary-pack ">B" 255)))
(test-equal "64-bit max round-trips"
            '(9223372036854775807)
            (binary-unpack ">q" (binary-pack ">q" 9223372036854775807)))

(test-section "(crab binary) — errors")
(test-true "value out of range for the code raises"
           (guard (e (#t #t)) (binary-pack ">b" 200) #f))
(test-true "wrong value count raises"
           (guard (e (#t #t)) (binary-pack ">ii" 1) #f))
(test-true "short buffer on unpack raises"
           (guard (e (#t #t)) (binary-unpack ">i" (binary-pack ">b" 1)) #f))
