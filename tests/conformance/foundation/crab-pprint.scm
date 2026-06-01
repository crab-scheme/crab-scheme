; Conformance test for `(crab pprint)` — pretty-printing.

; pretty-format is whitespace-formatted `write`, so reading its output
; back must yield the original datum — a format-independent invariant.
(define (roundtrip x)
  (read (open-input-string (pretty-format x))))

(define (compact x)
  (let ((p (open-output-string))) (write x p) (get-output-string p)))

(define (has-newline? s)
  (let loop ((i 0))
    (cond ((>= i (string-length s)) #f)
          ((char=? (string-ref s i) #\newline) #t)
          (else (loop (+ i 1))))))

(define wide '(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25))

(test-section "(crab pprint) — round-trip invariant")
(test-equal "flat list round-trips" '(1 2 3) (roundtrip '(1 2 3)))
(test-equal "nested list round-trips" '(a (b c) (d (e f))) (roundtrip '(a (b c) (d (e f)))))
(test-equal "vector round-trips" #(1 2 3) (roundtrip #(1 2 3)))
(test-equal "atom round-trips" 42 (roundtrip 42))
(test-equal "string round-trips" "hi" (roundtrip "hi"))
(test-equal "wide list round-trips unchanged" wide (roundtrip wide))

(test-section "(crab pprint) — layout")
(test-equal "short data prints compactly" (compact '(1 2 3)) (pretty-format '(1 2 3)))
(test-false "short data stays on one line" (has-newline? (pretty-format '(1 2 3))))
(test-true "wide data is broken across lines" (has-newline? (pretty-format wide)))
(test-true "pretty-format returns a string" (string? (pretty-format wide)))
