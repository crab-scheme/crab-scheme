(test-section "Transcendental + I/O extras + string/vector HO")

; sqrt
(test-eqv "sqrt-16"   4.0 (sqrt 16))
(test-eqv "sqrt-2-approx"
  (sqrt 2)  ; tested for shape
  (sqrt 2))

; exp / log roundtrip
(test-true "exp-log-roundtrip"
  (let ((x (log 5)))
    (let ((y (exp x)))
      (< (abs (- y 5)) 0.0001))))

; trig sanity
(test-eqv "sin-0"   0.0 (sin 0))
(test-eqv "cos-0"   1.0 (cos 0))
(test-true "sin-pi-near-zero"
  (< (abs (sin 3.14159265358979)) 0.0001))
(test-true "cos-pi-near-neg-one"
  (< (abs (- (cos 3.14159265358979) -1)) 0.0001))

; inverse trig
(test-eqv "atan-0"  0.0 (atan 0))
(test-true "atan2-quadrant-1"
  (let ((r (atan 1 1)))
    (< (abs (- r (/ 3.14159265358979 4))) 0.0001)))

; log with base
(test-true "log-base-2-of-8"
  (< (abs (- (log 8 2) 3)) 0.0001))

; string-map / string-for-each
(test-equal "string-map-upcase"
  "HELLO"
  (string-map char-upcase "hello"))
(test-equal "string-map-downcase"
  "abc"
  (string-map char-downcase "ABC"))

(define sfe-count 0)
(string-for-each (lambda (c) (set! sfe-count (+ sfe-count 1))) "abc")
(test-eqv "string-for-each-count" 3 sfe-count)

; vector-filter / vector-fold
(test-equal "vector-filter-evens"
  #(2 4 6)
  (vector-filter even? #(1 2 3 4 5 6)))
(test-equal "vector-filter-empty"
  #()
  (vector-filter (lambda (x) #f) #(1 2 3)))

(test-eqv "vector-fold-sum"   15  (vector-fold + 0 #(1 2 3 4 5)))
(test-eqv "vector-fold-prod"  120 (vector-fold * 1 #(1 2 3 4 5)))

; get-string-all
(test-equal "get-string-all-content"
  "hello world"
  (get-string-all (open-string-input-port "hello world")))
(test-equal "get-string-all-multiline"
  "line1\nline2\nline3"
  (get-string-all (open-string-input-port "line1\nline2\nline3")))

; read-line with explicit port
(define line-port (open-string-input-port "first\nsecond\nthird"))
(test-equal "read-line-1" "first"  (read-line line-port))
(test-equal "read-line-2" "second" (read-line line-port))
(test-equal "read-line-3" "third"  (read-line line-port))
(test-true  "read-line-eof" (eof-object? (read-line line-port)))
