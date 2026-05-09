(test-section "R7RS case-insensitive char-ci-* and string-ci-* compares")

; --- char-ci=? ---
(test-true  "char-ci=-aA"     (char-ci=? #\a #\A))
(test-true  "char-ci=-Bb"     (char-ci=? #\B #\b))
(test-false "char-ci=-aB"     (char-ci=? #\a #\B))
(test-true  "char-ci=-3args"  (char-ci=? #\a #\A #\a))
(test-false "char-ci=-3-mix"  (char-ci=? #\a #\A #\b))
(test-true  "char-ci=-1arg"   (char-ci=? #\x))
(test-true  "char-ci=-0arg"   (char-ci=?))

; --- char-ci<? ---
(test-true  "char-ci<-aB"     (char-ci<? #\a #\B))
(test-true  "char-ci<-Ab"     (char-ci<? #\A #\b))
(test-false "char-ci<-equal"  (char-ci<? #\a #\A))
(test-false "char-ci<-Ba"     (char-ci<? #\B #\a))

; --- char-ci<=? ---
(test-true  "char-ci<=-equal" (char-ci<=? #\a #\A))
(test-true  "char-ci<=-aB"    (char-ci<=? #\a #\B))
(test-false "char-ci<=-Ba"    (char-ci<=? #\B #\a))

; --- char-ci>? ---
(test-true  "char-ci>-Ba"     (char-ci>? #\B #\a))
(test-false "char-ci>-equal"  (char-ci>? #\a #\A))

; --- char-ci>=? ---
(test-true  "char-ci>=-equal" (char-ci>=? #\a #\A))
(test-true  "char-ci>=-Ba"    (char-ci>=? #\B #\a))
(test-false "char-ci>=-aB"    (char-ci>=? #\a #\B))

; --- string-ci=? ---
(test-true  "string-ci=-Hello-HELLO"      (string-ci=? "Hello" "HELLO"))
(test-true  "string-ci=-mixed-3"          (string-ci=? "abc" "ABC" "AbC"))
(test-false "string-ci=-different"        (string-ci=? "abc" "abd"))
(test-true  "string-ci=-empty"            (string-ci=? "" ""))
(test-true  "string-ci=-1arg"             (string-ci=? "anything"))
(test-false "string-ci=-len-mismatch"     (string-ci=? "abc" "abcd"))

; --- string-ci<? ---
(test-true  "string-ci<-Apple-banana"     (string-ci<? "Apple" "banana"))
(test-true  "string-ci<-apple-Banana"     (string-ci<? "apple" "Banana"))
(test-false "string-ci<-banana-Apple"     (string-ci<? "banana" "Apple"))
(test-false "string-ci<-equal"            (string-ci<? "Hello" "HELLO"))

; --- string-ci<=? ---
(test-true  "string-ci<=-equal-cases"     (string-ci<=? "Hello" "hello"))
(test-true  "string-ci<=-prefix"          (string-ci<=? "abc" "ABCD"))
(test-false "string-ci<=-greater"         (string-ci<=? "xyz" "abc"))

; --- string-ci>? ---
(test-true  "string-ci>-banana-Apple"     (string-ci>? "banana" "Apple"))
(test-false "string-ci>-equal"            (string-ci>? "Hello" "HELLO"))
(test-false "string-ci>-Apple-banana"     (string-ci>? "Apple" "banana"))

; --- string-ci>=? ---
(test-true  "string-ci>=-equal"           (string-ci>=? "Hello" "HELLO"))
(test-true  "string-ci>=-greater"         (string-ci>=? "banana" "Apple"))
(test-false "string-ci>=-less"            (string-ci>=? "Apple" "banana"))

; --- non-char/string args raise ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (char-ci=? #\a 42))))))
(test-eqv "char-ci=-non-char" 'caught c1)

(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (string-ci=? "abc" 42))))))
(test-eqv "string-ci=-non-string" 'caught c2)

; --- chained 3+ args for ordering ---
(test-true "string-ci<-3args"  (string-ci<? "Apple" "banana" "Cherry"))
(test-false "string-ci<-3args-not" (string-ci<? "Apple" "Cherry" "banana"))
(test-true "char-ci<-4args"    (char-ci<? #\a #\B #\c #\D))
