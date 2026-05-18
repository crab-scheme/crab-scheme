; Conformance test for `(crab base)` — stdlib-modules iter 6.

(define (bv-from-list ls)
  (let* ((n (length ls)) (bv (make-bytevector n 0)))
    (let loop ((i 0) (rest ls))
      (cond ((null? rest) bv)
            (else (bytevector-u8-set! bv i (car rest))
                  (loop (+ i 1) (cdr rest)))))))

(test-section "(crab base) — base64 standard")

(define __bv__ (bv-from-list '(1 2 3 4 5)))
(define __b64__ (base64-encode __bv__))
(test-true  "encode returns a string" (string? __b64__))
(test-eqv   "round-trip length matches"
            5
            (bytevector-length (base64-decode __b64__)))

(test-equal "encode \"hello\" bytes"
            "aGVsbG8="
            (base64-encode (bv-from-list '(104 101 108 108 111))))

(test-section "(crab base) — base64 url-safe")

(test-equal "url-safe encode no padding"
            "AQIDBAU"
            (base64url-encode (bv-from-list '(1 2 3 4 5))))
(test-eqv "url-safe round-trip"
          5
          (bytevector-length (base64url-decode (base64url-encode __bv__))))

(test-section "(crab base) — hex")

(test-equal "hex encode"
            "0102030405"
            (hex-encode (bv-from-list '(1 2 3 4 5))))
(test-equal "hex encode \"hello\""
            "68656c6c6f"
            (hex-encode (bv-from-list '(104 101 108 108 111))))

(test-eqv "hex decode length"
          5
          (bytevector-length (hex-decode "0102030405")))
