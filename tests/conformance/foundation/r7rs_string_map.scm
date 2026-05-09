(test-section "R7RS string-map / string-for-each multi-string forms")

; --- single-string form (R6RS-compatible) ---
(test-equal "smap-single" "ABC"
  (string-map char-upcase "abc"))

(test-equal "smap-id-empty" ""
  (string-map char-upcase ""))

; --- two-string form: pairwise on chars (interleave a, b) ---
(test-equal "smap-2str-eq"
  "AdBeCf"
  (let ((acc '()))
    (string-for-each
      (lambda (a b)
        (set! acc (cons a acc))
        (set! acc (cons b acc)))
      "ABC"
      "def")
    (list->string (reverse acc))))

; --- two-string form via string-map: combine each pair into a char ---
(test-equal "smap-2str-pick-first"
  "ABC"
  (string-map (lambda (a b) a) "ABC" "xyz"))

(test-equal "smap-2str-pick-second"
  "xyz"
  (string-map (lambda (a b) b) "ABC" "xyz"))

; --- shortest input bounds the result ---
(test-equal "smap-shortest-1"
  "A"
  (string-map (lambda (a b) a) "AB" "x"))

(test-equal "smap-shortest-2"
  "x"
  (string-map (lambda (a b) b) "ABC" "x"))

; --- three-string form ---
(test-equal "smap-3str"
  "abc."
  (string-map (lambda (a b c) c) "...." "...." "abc."))

; --- empty among inputs gives empty result ---
(test-equal "smap-empty-arg" ""
  (string-map (lambda (a b) a) "" "abc"))

; --- string-for-each side effects ---
(define collected '())
(string-for-each (lambda (c) (set! collected (cons c collected))) "xyz")
(test-equal "sfe-single" '(#\x #\y #\z) (reverse collected))

(define pairs '())
(string-for-each
  (lambda (a b) (set! pairs (cons (list a b) pairs)))
  "AB" "xy")
(test-equal "sfe-2str" '((#\A #\x) (#\B #\y)) (reverse pairs))

; --- single string-map preserves char->char mapping property ---
(define (rot1 c)
  (let ((i (char->integer c)))
    (integer->char (+ i 1))))
(test-equal "smap-rot1" "bcd" (string-map rot1 "abc"))

; --- proc must return char (not int) — error caught ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string-map (lambda (c) (char->integer c)) "abc"))))))
(test-eqv "smap-non-char-result" 'caught c1)

; --- non-string arg ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string-map char-upcase 42))))))
(test-eqv "smap-non-string-arg" 'caught c2)
