(test-section "R6RS hashtables: hashed bucket index correctness (cs-c7j)")

; --- 1000-key build + full probe (exercises the bucket index at scale) ---
(define big (make-hashtable))
(let loop ((i 0))
  (when (< i 1000)
    (hashtable-set! big i (* i i))
    (loop (+ i 1))))
(test-eqv "big-size" 1000 (hashtable-size big))
(let loop ((i 0) (ok #t))
  (if (< i 1000)
      (loop (+ i 1) (and ok (eqv? (* i i) (hashtable-ref big i #f))))
      (test-true "big-all-probe-ok" ok)))
(test-equal "big-miss" #f (hashtable-ref big 12345 #f))

; --- delete-then-probe: deleting a key must not disturb others, and the
;     deleted key must become unreachable (index rebuild correctness) ---
(hashtable-delete! big 500)
(test-eqv "after-delete-size" 999 (hashtable-size big))
(test-equal "deleted-key-gone" #f (hashtable-ref big 500 #f))
(test-eqv "neighbor-499-intact" 249001 (hashtable-ref big 499 #f))
(test-eqv "neighbor-501-intact" 251001 (hashtable-ref big 501 #f))
(test-eqv "last-key-still-there" 998001 (hashtable-ref big 999 #f))
; Re-inserting a deleted key must land in the (rebuilt) index too.
(hashtable-set! big 500 999)
(test-eqv "re-inserted-key" 999 (hashtable-ref big 500 #f))
(test-eqv "after-reinsert-size" 1000 (hashtable-size big))

; --- equal-keyed nested list keys (structural hash over pairs/vectors) ---
(define nested (make-hashtable))
(hashtable-set! nested (list 1 (list 2 3) "x") 'a)
(hashtable-set! nested (list 1 (list 2 4) "x") 'b)
(hashtable-set! nested (vector 1 2 3) 'c)
(test-eqv "nested-hit-a" 'a (hashtable-ref nested (list 1 (list 2 3) "x") #f))
(test-eqv "nested-hit-b" 'b (hashtable-ref nested (list 1 (list 2 4) "x") #f))
(test-eqv "nested-hit-c" 'c (hashtable-ref nested (vector 1 2 3) #f))
(test-equal "nested-miss" #f (hashtable-ref nested (list 1 (list 2 5) "x") #f))
; Overwriting via a distinct-but-equal? nested key updates in place, not append.
(hashtable-set! nested (list 1 (list 2 3) "x") 'a2)
(test-eqv "nested-overwrite" 'a2 (hashtable-ref nested (list 1 (list 2 3) "x") #f))
(test-eqv "nested-size-unchanged" 3 (hashtable-size nested))

; --- custom-equiv table still works with the hashed lookup path ---
(define (ci-hash s) (string-hash (string-foldcase s)))
(define (ci-equiv a b) (string=? (string-foldcase a) (string-foldcase b)))
(define ci (make-hashtable ci-hash ci-equiv))
(hashtable-set! ci "Alpha" 1)
(hashtable-set! ci "Beta" 2)
(test-eqv "ci-hit-lower" 1 (hashtable-ref ci "alpha" #f))
(test-eqv "ci-hit-upper" 2 (hashtable-ref ci "BETA" #f))
(test-true "ci-contains" (hashtable-contains? ci "ALPHA"))
(hashtable-delete! ci "beta")
(test-eqv "ci-after-delete-size" 1 (hashtable-size ci))
(test-equal "ci-deleted-gone" #f (hashtable-ref ci "Beta" #f))

; --- eqv?/hash edge cases: exactness matters, 2 and 2.0 are distinct keys ---
(define ev (make-eqv-hashtable))
(hashtable-set! ev 2 'exact)
(hashtable-set! ev 2.0 'inexact)
(test-eqv "eqv-exact-2"   'exact   (hashtable-ref ev 2 #f))
(test-eqv "eqv-inexact-2" 'inexact (hashtable-ref ev 2.0 #f))
(test-eqv "eqv-two-distinct-entries" 2 (hashtable-size ev))
(test-false "eqv?-2-vs-2.0-direct" (eqv? 2 2.0))

; Characters and fixnums hash/compare distinctly from each other too.
(hashtable-set! ev #\a 'char-a)
(test-eqv "eqv-char-a" 'char-a (hashtable-ref ev #\a #f))
(test-eqv "eqv-size-after-char" 3 (hashtable-size ev))
