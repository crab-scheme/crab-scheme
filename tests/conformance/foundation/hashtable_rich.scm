(test-section "R6RS hashtable rich API: copy, mutable?, equivalence/hash, entries")

; --- hashtable-copy ---
(define h1 (make-eq-hashtable))
(hashtable-set! h1 'a 1)
(hashtable-set! h1 'b 2)
(define h1-copy (hashtable-copy h1))
(test-eqv "copy preserves size" 2 (hashtable-size h1-copy))
(test-eqv "copy preserves a"    1 (hashtable-ref h1-copy 'a #f))
(test-eqv "copy preserves b"    2 (hashtable-ref h1-copy 'b #f))

; Mutating the copy doesn't affect the original.
(hashtable-set! h1-copy 'a 99)
(hashtable-set! h1-copy 'c 3)
(test-eqv "original a unchanged" 1 (hashtable-ref h1 'a #f))
(test-eqv "original size unchanged" 2 (hashtable-size h1))
(test-eqv "copy a updated" 99 (hashtable-ref h1-copy 'a #f))
(test-eqv "copy size grew" 3 (hashtable-size h1-copy))

; --- hashtable-mutable? ---
(test-true "mutable? on copy"  (hashtable-mutable? h1))
(test-true "mutable? on copy"  (hashtable-mutable? h1-copy))

; --- hashtable-equivalence-function ---
; The returned procedure should behave like the configured equivalence.
(define eq-fn (hashtable-equivalence-function h1))
(test-true "eq-fn applies"        (eq-fn 'a 'a))
(test-false "eq-fn rejects strs"  (eq-fn "x" "x"))   ; different string objects

(define equal-ht (make-hashtable))
(define eq-fn2 (hashtable-equivalence-function equal-ht))
(test-true  "equal-fn on lists"    (eq-fn2 '(1 2) '(1 2)))
(test-true  "equal-fn on strings"  (eq-fn2 "x" "x"))

; --- hashtable-hash-function ---
; For built-in eq/eqv/equal hashtables, returns #f (no custom hash).
(test-equal "hash-fn eq → #f"     #f (hashtable-hash-function h1))
(test-equal "hash-fn equal → #f"  #f (hashtable-hash-function equal-ht))

; --- hashtable-entries: returns 2 values, both vectors, parallel ---
(define h2 (make-eqv-hashtable))
(hashtable-set! h2 'x 10)
(hashtable-set! h2 'y 20)
(hashtable-set! h2 'z 30)

(call-with-values
  (lambda () (hashtable-entries h2))
  (lambda (ks vs)
    (test-true  "entries keys is vector"  (vector? ks))
    (test-true  "entries vals is vector"  (vector? vs))
    (test-eqv   "entries keys length"  3 (vector-length ks))
    (test-eqv   "entries vals length"  3 (vector-length vs))
    ; verify pairing: every (ks[i] -> vs[i]) is a real entry in h2
    (let loop ((i 0))
      (if (= i (vector-length ks))
          (test-true "entries fully paired" #t)
          (begin
            (test-eqv (string-append "pair check " (number->string i))
              (hashtable-ref h2 (vector-ref ks i) #f)
              (vector-ref vs i))
            (loop (+ i 1)))))))

; --- empty hashtable: entries returns two empty vectors ---
(define empty-ht (make-hashtable))
(call-with-values
  (lambda () (hashtable-entries empty-ht))
  (lambda (ks vs)
    (test-eqv "empty entries keys" 0 (vector-length ks))
    (test-eqv "empty entries vals" 0 (vector-length vs))))
