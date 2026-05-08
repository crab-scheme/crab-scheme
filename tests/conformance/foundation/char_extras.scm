(test-section "Character extras: case + predicates")

; case conversion
(test-eqv "char-upcase-a"   #\A  (char-upcase #\a))
(test-eqv "char-upcase-z"   #\Z  (char-upcase #\z))
(test-eqv "char-upcase-A"   #\A  (char-upcase #\A))    ; idempotent on upper
(test-eqv "char-downcase-A" #\a  (char-downcase #\A))
(test-eqv "char-downcase-Z" #\z  (char-downcase #\Z))
(test-eqv "char-downcase-a" #\a  (char-downcase #\a))   ; idempotent on lower

; alphabetic / numeric / whitespace
(test-true  "alphabetic-a"  (char-alphabetic? #\a))
(test-true  "alphabetic-Z"  (char-alphabetic? #\Z))
(test-false "alphabetic-1"  (char-alphabetic? #\1))
(test-false "alphabetic-sp" (char-alphabetic? #\space))
(test-true  "numeric-0"     (char-numeric? #\0))
(test-true  "numeric-9"     (char-numeric? #\9))
(test-false "numeric-a"     (char-numeric? #\a))
(test-true  "whitespace-sp" (char-whitespace? #\space))
(test-true  "whitespace-tab" (char-whitespace? #\tab))
(test-true  "whitespace-nl" (char-whitespace? #\newline))
(test-false "whitespace-a"  (char-whitespace? #\a))

; case predicates
(test-true  "upper-A"       (char-upper-case? #\A))
(test-false "upper-a"       (char-upper-case? #\a))
(test-true  "lower-a"       (char-lower-case? #\a))
(test-false "lower-A"       (char-lower-case? #\A))

; eof
(test-true  "eof-of-eof-object"  (eof-object? (eof-object)))
(test-false "eof-of-num"          (eof-object? 42))
(test-false "eof-of-symbol"       (eof-object? (quote eof)))

; symbol <-> string
(test-equal "sym->str"   "hello"  (symbol->string (quote hello)))
(test-eqv   "str->sym"   (quote hello) (string->symbol "hello"))
(test-eqv   "roundtrip"  (quote foo) (string->symbol (symbol->string (quote foo))))
