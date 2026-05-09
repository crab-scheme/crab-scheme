(test-section "R7RS assoc/member with optional comparison procedure")

; --- 2-arg form unchanged ---
(test-equal "assoc-2-arg" '("b" 2) (assoc "b" '(("a" 1) ("b" 2) ("c" 3))))
(test-equal "member-2-arg" '("b" "c") (member "b" '("a" "b" "c")))

; --- 3-arg assoc with custom predicate ---
; Case-insensitive string match.
(define (ci-eq a b)
  (string=? (string-foldcase a) (string-foldcase b)))
(test-equal "assoc-ci" '("Bob" 2)
  (assoc "BOB" '(("alice" 1) ("Bob" 2) ("CAROL" 3)) ci-eq))

; --- 3-arg member with custom predicate ---
(test-equal "member-ci" '("Bob" "Carol")
  (member "BOB" '("alice" "Bob" "Carol") ci-eq))

; Numeric tolerance: find a "close enough" key
(define (close-to a b) (< (abs (- a b)) 0.5))
(test-equal "assoc-tolerance" '(2.0 second)
  (assoc 1.9 '((1.0 first) (2.0 second) (3.0 third)) close-to))

; --- 3-arg with no match → #f ---
(test-equal "assoc-no-match" #f
  (assoc "zzz" '(("a" 1) ("b" 2)) ci-eq))
(test-equal "member-no-match" #f
  (member "zzz" '("a" "b" "c") ci-eq))

; --- 3-arg with empty list → #f ---
(test-equal "assoc-empty" #f (assoc 'k '() ci-eq))
(test-equal "member-empty" #f (member 'k '() ci-eq))

; --- predicate is called on (head, key) order ---
(define (left-greater a b) (> a b))
; Find first list element > 5
(test-equal "member-pred-order" '(7 8 9)
  (member 5 '(1 2 3 7 8 9) left-greater))

; --- 4-arg arity error ---
(test-true "assoc-4-arg-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (assoc 'k '() ci-eq 'extra))))
(test-true "member-4-arg-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (member 'k '() ci-eq 'extra))))

; --- assq/assv/memq/memv don't take the 3rd arg (R6RS-only) ---
(test-true "assq-3-arg-raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (assq 'a '((a 1)) eq?))))
