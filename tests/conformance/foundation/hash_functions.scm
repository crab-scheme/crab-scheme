(test-section "R6RS hash functions: string-hash / symbol-hash / equal-hash")

; --- determinism ---
(test-eqv "string-hash-deterministic"
  (string-hash "hello") (string-hash "hello"))
(test-eqv "symbol-hash-deterministic"
  (symbol-hash 'foo) (symbol-hash 'foo))
(test-eqv "equal-hash-deterministic"
  (equal-hash '(1 2 3)) (equal-hash '(1 2 3)))

; --- type errors raise proper conditions ---
(test-true "string-hash-rejects-num"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'string-hash)))
    (lambda () (string-hash 42))))
(test-true "symbol-hash-rejects-string"
  (with-exception-handler
    (lambda (c) (and (error? c) (eq? (condition-who c) 'symbol-hash)))
    (lambda () (symbol-hash "not-a-symbol"))))

; --- different inputs hash differently (no false collisions on small set) ---
(define hashes
  (map string-hash '("a" "b" "c" "ab" "ba" "abc" "")))
(test-eqv "string-hash-distinct"
  (length hashes) (length (delete-duplicates hashes)))

; --- equal-hash respects equal? semantics on compound structures ---
(define a (list 1 2 (list 3 4)))
(define b (list 1 2 (list 3 4)))
(test-true  "equal-hash-pair-equal?"  (equal? a b))
(test-eqv   "equal-hash-pair"         (equal-hash a) (equal-hash b))

(test-eqv "equal-hash-vector"
  (equal-hash #(a b c)) (equal-hash #(a b c)))

; --- usable as the hash arg to make-hashtable (we ignore it but the
; symbols must resolve so R6RS programs compile cleanly) ---
(define h (make-hashtable string-hash equal?))
(hashtable-set! h "key" 42)
(test-eqv "ht-roundtrip" 42 (hashtable-ref h "key" #f))

; --- all results are non-negative fixnums (programs may use them in
; modular arithmetic without sign concerns) ---
(test-true "string-hash-nonneg"  (>= (string-hash "x") 0))
(test-true "symbol-hash-nonneg"  (>= (symbol-hash 'x) 0))
(test-true "equal-hash-nonneg"   (>= (equal-hash 'x) 0))
