(test-section "R7RS |pipe-quoted| identifiers")

; --- basic pipe-quoted symbols are valid identifiers ---
(define |hello world| 42)
(test-eqv "pipe-ident-define" 42 |hello world|)

; --- pipe-quoted symbol equals same name produced via string->symbol ---
(test-true "pipe-ident-string->symbol-eq"
  (eq? '|abc def| (string->symbol "abc def")))

; --- empty pipe-quoted symbol ---
(test-true "pipe-ident-empty"
  (eq? '|| (string->symbol "")))

; --- pipe-quoted symbol with special chars (non-identifier-initial) ---
(test-true "pipe-ident-digit-start"
  (eq? '|1+1| (string->symbol "1+1")))

; --- pipe-quoted symbol with arithmetic-looking name ---
(test-true "pipe-ident-arith"
  (eq? '|a b c| (string->symbol "a b c")))

; --- symbol->string round-trip ---
(test-equal "pipe-symbol->string"
  "this is a symbol"
  (symbol->string '|this is a symbol|))

; --- hex escape inside pipe ---
(test-equal "pipe-hex-escape"
  "ABC"
  (symbol->string '|\x41;\x42;\x43;|))

; --- escaped pipe inside pipe ---
(test-equal "pipe-escaped-pipe"
  "a|b"
  (symbol->string '|a\|b|))

; --- escaped backslash inside pipe ---
(test-equal "pipe-escaped-backslash"
  "a\\b"
  (symbol->string '|a\\b|))

; --- two adjacent pipe-quoted symbols ---
(define |my var one|  10)
(define |my var two|  20)
(test-eqv "pipe-multi-1" 10 |my var one|)
(test-eqv "pipe-multi-2" 20 |my var two|)

; --- pipe-quoted symbol can be used as a variable in expressions ---
(define (|make adder| n)
  (lambda (x) (+ x n)))
(define |+5| (|make adder| 5))
(test-eqv "pipe-as-fn-name" 12 (|+5| 7))

; --- pipe-quoted name with whitespace mixed in calls ---
(define (|min of three| a b c)
  (if (< a b)
      (if (< a c) a c)
      (if (< b c) b c)))
(test-eqv "pipe-fn-args" 1 (|min of three| 3 1 2))

; --- pipe-quoted name in let binding ---
(test-eqv "pipe-in-let" 99
  (let ((|some name| 99)) |some name|))

; --- pipe-quoted symbol with unicode char ---
(test-true "pipe-unicode"
  (eq? '|π| (string->symbol "π")))
