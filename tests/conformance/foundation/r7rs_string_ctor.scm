(test-section "R7RS (string char ...) constructor")

; --- empty ---
(test-equal "string-ctor-empty" "" (string))

; --- single character ---
(test-equal "string-ctor-1" "a" (string #\a))

; --- multi character ---
(test-equal "string-ctor-3" "abc" (string #\a #\b #\c))

; --- ascii + unicode mix ---
(test-equal "string-ctor-utf8" "Hαβ" (string #\H #\α #\β))

; --- whitespace and special chars ---
(test-equal "string-ctor-special"
  "a b\tc\nd"
  (string #\a #\space #\b #\tab #\c #\newline #\d))

; --- Length agrees ---
(test-eqv "string-ctor-len-3" 3 (string-length (string #\x #\y #\z)))
(test-eqv "string-ctor-len-0" 0 (string-length (string)))

; --- Round trip with string->list ---
(test-equal "string-ctor-roundtrip" '(#\h #\i)
  (string->list (string #\h #\i)))

; --- Round trip with list->string ---
(test-equal "string-ctor-vs-list->string"
  (list->string '(#\a #\b #\c))
  (string #\a #\b #\c))

; --- Type error on non-character ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string #\a 42 #\b))))))
(test-eqv "string-ctor-non-char" 'caught c1)

; --- usable in expressions / let ---
(define (greeting name)
  (string-append "Hello, " name "!"))
(test-equal "string-ctor-in-fn"
  "Hello, X!"
  (greeting (string #\X)))

; --- nesting ---
(test-equal "string-ctor-nested"
  "ABC"
  (string-append (string #\A) (string #\B #\C)))
