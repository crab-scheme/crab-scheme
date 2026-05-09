(test-section "make-hashtable with user-supplied hash + equivalence procs")

; Build a case-insensitive string hashtable: keys compare with
; string-ci=? and hash via string-foldcase.
(define (ci-hash s) (string-hash (string-foldcase s)))
(define (ci-equiv a b)
  (string=? (string-foldcase a) (string-foldcase b)))

(define ht (make-hashtable ci-hash ci-equiv))

(hashtable-set! ht "Hello" 1)
(hashtable-set! ht "World" 2)

; Looking up with a different-case spelling finds the same entry.
(test-eqv "ci-ref hello" 1 (hashtable-ref ht "hello" #f))
(test-eqv "ci-ref HELLO" 1 (hashtable-ref ht "HELLO" #f))
(test-eqv "ci-ref world" 2 (hashtable-ref ht "world" #f))

; contains? uses the custom equiv too.
(test-true  "ci-contains hello" (hashtable-contains? ht "hello"))
(test-true  "ci-contains HELLO" (hashtable-contains? ht "HELLO"))
(test-false "ci-contains other" (hashtable-contains? ht "other"))

; Setting with a different-case key updates the existing entry, not new.
(hashtable-set! ht "hello" 11)
(test-eqv "ci-set updates"   11 (hashtable-ref ht "Hello" #f))
(test-eqv "ci-size"           2 (hashtable-size ht))

; Delete with a different-case key removes the canonical entry.
(hashtable-delete! ht "WORLD")
(test-eqv "ci-after-delete-size" 1 (hashtable-size ht))
(test-equal "ci-after-delete-ref" #f (hashtable-ref ht "world" #f))

; --- inspection ops return the user-supplied procs ---
(test-true  "hash-fn returned"  (procedure? (hashtable-hash-function ht)))
(test-true  "equiv-fn returned" (procedure? (hashtable-equivalence-function ht)))

; The returned equiv-fn applied to two values gives the same result
; as our ci-equiv.
(define stored-equiv (hashtable-equivalence-function ht))
(test-true  "stored-equiv abc=ABC" (stored-equiv "abc" "ABC"))
(test-false "stored-equiv abc=xyz" (stored-equiv "abc" "xyz"))

; --- copy preserves the custom hash + equiv ---
(define ht2 (hashtable-copy ht))
(test-eqv "copy ci-ref" 11 (hashtable-ref ht2 "HELLO" #f))
(test-true "copy keeps custom equiv"
  (procedure? (hashtable-equivalence-function ht2)))

; --- arity error: 1-arg make-hashtable raises ---
(test-true "make-hashtable 1 arg raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (make-hashtable ci-hash))))

; --- non-procedure args raise ---
(test-true "make-hashtable non-proc hash raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (make-hashtable 42 ci-equiv))))
(test-true "make-hashtable non-proc equiv raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (make-hashtable ci-hash 42))))

; --- a numeric custom-equiv: keys compare as integers within tolerance ---
(define (int-hash n) (abs (exact (round n))))
(define (close-equiv a b) (< (abs (- a b)) 0.5))
(define ntab (make-hashtable int-hash close-equiv))
(hashtable-set! ntab 1.0 'one)
(hashtable-set! ntab 2.0 'two)
(test-equal "close-ref 1.2" 'one (hashtable-ref ntab 1.2 #f))
(test-equal "close-ref 1.7" 'two (hashtable-ref ntab 1.7 #f))
(test-equal "close-ref 5.0"  #f  (hashtable-ref ntab 5.0 #f))
