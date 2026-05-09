(test-section "R7RS vector-fill!, string-fill! with optional start/end")

; --- vector-fill! whole vector (R6RS-style 2-arg) ---
(define v1 (vector 1 2 3 4 5))
(vector-fill! v1 0)
(test-equal "vfill-whole" '#(0 0 0 0 0) v1)

; --- vector-fill! with start ---
(define v2 (vector 1 2 3 4 5))
(vector-fill! v2 'X 2)
(test-equal "vfill-start" '#(1 2 X X X) v2)

; --- vector-fill! with start + end ---
(define v3 (vector 1 2 3 4 5))
(vector-fill! v3 'Y 1 3)
(test-equal "vfill-start-end" '#(1 Y Y 4 5) v3)

; --- vector-fill! empty range (start = end) ---
(define v4 (vector 1 2 3))
(vector-fill! v4 'Z 1 1)
(test-equal "vfill-empty-range" '#(1 2 3) v4)

; --- vector-fill! at boundaries ---
(define v5 (vector 1 2 3 4))
(vector-fill! v5 9 0 0)
(test-equal "vfill-zero-zero" '#(1 2 3 4) v5)

(define v6 (vector 1 2 3 4))
(vector-fill! v6 9 4 4)
(test-equal "vfill-end-end" '#(1 2 3 4) v6)

; --- vector-fill! out-of-range start error ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (vector-fill! (vector 1 2 3) 'x 5))))))
(test-eqv "vfill-bad-start" 'caught c1)

(test-section "string-fill!")

; --- string-fill! whole string (R6RS 2-arg) ---
(define s1 (make-string 5 #\a))
(string-fill! s1 #\Z)
(test-equal "sfill-whole" "ZZZZZ" s1)

; --- string-fill! with start ---
(define s2 (make-string 6 #\x))
(string-fill! s2 #\Y 3)
(test-equal "sfill-start" "xxxYYY" s2)

; --- string-fill! with start + end ---
(define s3 (make-string 6 #\.))
(string-fill! s3 #\* 1 4)
(test-equal "sfill-start-end" ".***.." s3)

; --- string-fill! empty range ---
(define s4 (string-copy "hello"))
(string-fill! s4 #\Q 2 2)
(test-equal "sfill-empty-range" "hello" s4)

; --- string-fill! type error: not a char ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string-fill! (make-string 3) 42))))))
(test-eqv "sfill-bad-type" 'caught c2)

; --- string-fill! arity error ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string-fill! (make-string 3) #\a 0 1 2))))))
(test-eqv "sfill-arity" 'caught c3)

; --- string-fill! roundtrip via string->list ---
(define s5 (make-string 4 #\x))
(string-fill! s5 #\!)
(test-equal "sfill-roundtrip" '(#\! #\! #\! #\!) (string->list s5))
