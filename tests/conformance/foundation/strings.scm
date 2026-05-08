(test-section "R6RS §11.12 — strings")

; Construction
(test-equal "make-string-empty"  ""        (make-string 0))
(test-equal "make-string-fill"   "aaa"     (make-string 3 #\a))
(test-equal "make-string-default" "   "    (make-string 3))

; Length
(test-eqv "string-length-empty"  0  (string-length ""))
(test-eqv "string-length-5"      5  (string-length "hello"))
(test-eqv "string-length-unicode" 4 (string-length "café"))

; Indexing
(test-eqv "string-ref-0"   #\h    (string-ref "hello" 0))
(test-eqv "string-ref-4"   #\o    (string-ref "hello" 4))

; Equality
(test-true  "string-eq"     (string=? "abc" "abc"))
(test-false "string-eq-not" (string=? "abc" "abd"))
(test-true  "string-eq-multi" (string=? "x" "x" "x"))

; Concatenation
(test-equal "string-append"     "hello world"  (string-append "hello" " " "world"))
(test-equal "string-append-empty" ""           (string-append))
(test-equal "string-append-one" "abc"          (string-append "abc"))

; Substring
(test-equal "substring"        "ell"   (substring "hello" 1 4))
(test-equal "substring-full"   "abc"   (substring "abc" 0 3))
(test-equal "substring-empty"  ""      (substring "abc" 1 1))

; Conversion to/from list
(test-equal "string->list"     '(#\a #\b #\c) (string->list "abc"))
(test-equal "list->string"     "abc"           (list->string '(#\a #\b #\c)))
(test-equal "string-roundtrip" "hello"         (list->string (string->list "hello")))

; Number ↔ string
(test-equal "number->string-10" "42"      (number->string 42))
(test-equal "number->string-hex" "ff"     (number->string 255 16))
(test-equal "number->string-bin" "1010"   (number->string 10 2))
(test-eqv   "string->number-10"  42       (string->number "42"))
(test-eqv   "string->number-hex" 255      (string->number "ff" 16))
(test-eqv   "string->number-bad" #f       (string->number "not a number"))

; Copy
(test-equal "string-copy"  "hello"  (string-copy "hello"))
