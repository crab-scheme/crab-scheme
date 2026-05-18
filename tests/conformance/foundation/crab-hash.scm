; Conformance test for `(crab hash)` — stdlib-modules iter 7.

(test-section "(crab hash) — digest lengths")

(test-eqv "sha256 length"  32 (bytevector-length (hash-sha256 "")))
(test-eqv "sha512 length"  64 (bytevector-length (hash-sha512 "")))
(test-eqv "sha1 length"    20 (bytevector-length (hash-sha1 "")))
(test-eqv "md5 length"     16 (bytevector-length (hash-md5 "")))
(test-eqv "blake3 length"  32 (bytevector-length (hash-blake3 "")))

(test-section "(crab hash) — known vectors")

;; "abc" SHA-256 = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
(test-equal "sha256 \"abc\" first 4 bytes"
            '(186 120 22 191)
            (let ((bv (hash-sha256 "abc")))
              (list (bytevector-u8-ref bv 0)
                    (bytevector-u8-ref bv 1)
                    (bytevector-u8-ref bv 2)
                    (bytevector-u8-ref bv 3))))

;; "" MD5 = d41d8cd98f00b204e9800998ecf8427e
(test-eqv "md5 empty first byte"  212 (bytevector-u8-ref (hash-md5 "") 0))
(test-eqv "md5 empty second byte"  29 (bytevector-u8-ref (hash-md5 "") 1))

(test-section "(crab hash) — string vs bv equivalence")

(define __bv-hi__ (let ((b (make-bytevector 2 0)))
                    (bytevector-u8-set! b 0 104)
                    (bytevector-u8-set! b 1 105)
                    b))
(test-equal "sha256 of string == sha256 of bv"
            (hash-sha256 "hi")
            (hash-sha256 __bv-hi__))

(test-section "(crab hash) — hmac")

(test-eqv "hmac-sha256 length"  32
          (bytevector-length (hmac-sha256 "secret" "message")))

;; Empty key + empty msg is a stable vector
;; b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad
(test-eqv "hmac-sha256 empty/empty first byte"
          182
          (bytevector-u8-ref (hmac-sha256 "" "") 0))
