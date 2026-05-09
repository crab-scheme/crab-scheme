(test-section "R7RS *->list / *->vector / *->string with optional [start [end]]")

; --- string->list with no extra args (R6RS-compatible) ---
(test-equal "stl-full" '(#\h #\i) (string->list "hi"))

; --- string->list with start ---
(test-equal "stl-start" '(#\l #\o) (string->list "hello" 3))

; --- string->list with start + end ---
(test-equal "stl-start-end" '(#\b #\c #\d) (string->list "abcdef" 1 4))

; --- string->list empty slice ---
(test-equal "stl-empty" '() (string->list "abc" 1 1))

; --- string->list with multibyte UTF-8 ---
(test-equal "stl-utf8-full" '(#\α #\β #\γ) (string->list "αβγ"))
(test-equal "stl-utf8-mid"  '(#\β) (string->list "αβγ" 1 2))

; --- vector->list with no extra args ---
(test-equal "vtl-full" '(1 2 3 4 5) (vector->list (vector 1 2 3 4 5)))

; --- vector->list with start ---
(test-equal "vtl-start" '(3 4 5) (vector->list (vector 1 2 3 4 5) 2))

; --- vector->list with start + end ---
(test-equal "vtl-start-end" '(2 3 4) (vector->list (vector 1 2 3 4 5) 1 4))

; --- vector->list empty slice ---
(test-equal "vtl-empty" '() (vector->list (vector 1 2 3) 1 1))

; --- string->vector ---
(test-equal "stv-full" '#(#\a #\b #\c) (string->vector "abc"))
(test-equal "stv-start" '#(#\b #\c #\d) (string->vector "abcde" 1 4))

; --- vector->string ---
(test-equal "vts-full"
  "abc"
  (vector->string (vector #\a #\b #\c)))

(test-equal "vts-start-end"
  "bc"
  (vector->string (vector #\a #\b #\c #\d) 1 3))

; --- vector->string non-character element errors ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (vector->string (vector #\a 42 #\c)))))))
(test-eqv "vts-non-char" 'caught c1)

; --- out-of-range errors ---
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string->list "abc" 99))))))
(test-eqv "stl-bad-start" 'caught c2)

(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (vector->list (vector 1 2 3) 2 1))))))
(test-eqv "vtl-end-before-start" 'caught c3)

; --- nested round-trips ---
(test-equal "rt-vec-str" "world"
  (vector->string (string->vector "hello world" 6)))

(test-equal "rt-str-vec-list"
  '(#\X #\Y)
  (vector->list (string->vector "WXYZ" 1 3)))
