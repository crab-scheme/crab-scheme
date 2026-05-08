(test-section "R6RS hashtables")

; Construction
(define h (make-eq-hashtable))
(test-true  "hashtable?-of-eq-ht"  (hashtable? h))
(test-false "hashtable?-of-list"   (hashtable? '(1 2 3)))
(test-eqv   "ht-empty-size"  0  (hashtable-size h))

; Set / ref
(hashtable-set! h 'a 1)
(hashtable-set! h 'b 2)
(hashtable-set! h 'c 3)
(test-eqv "ht-size-3"        3   (hashtable-size h))
(test-eqv "ht-ref-a"         1   (hashtable-ref h 'a #f))
(test-eqv "ht-ref-b"         2   (hashtable-ref h 'b #f))
(test-eqv "ht-ref-missing"   #f  (hashtable-ref h 'missing #f))
(test-eqv "ht-ref-default"   42  (hashtable-ref h 'missing 42))

; Update existing key
(hashtable-set! h 'a 100)
(test-eqv "ht-ref-updated"   100 (hashtable-ref h 'a #f))
(test-eqv "ht-size-after-update" 3 (hashtable-size h))

; contains?
(test-true  "ht-contains-a"  (hashtable-contains? h 'a))
(test-false "ht-contains-missing" (hashtable-contains? h 'missing))

; delete!
(hashtable-delete! h 'b)
(test-eqv   "ht-size-after-delete" 2 (hashtable-size h))
(test-false "ht-no-b-after-delete" (hashtable-contains? h 'b))

; clear!
(hashtable-clear! h)
(test-eqv "ht-empty-after-clear" 0 (hashtable-size h))

; eqv? based hashtable: numbers compared by value
(define h2 (make-eqv-hashtable))
(hashtable-set! h2 5 'five)
(test-eqv "eqv-ht-num-key"   'five  (hashtable-ref h2 5 #f))

; equal? based hashtable: structural keys work
(define h3 (make-hashtable))
(hashtable-set! h3 '(1 2) 'pair-one-two)
(test-eqv "equal-ht-list-key" 'pair-one-two
  (hashtable-ref h3 '(1 2) #f))

; keys / values
(define h4 (make-eq-hashtable))
(hashtable-set! h4 'x 10)
(hashtable-set! h4 'y 20)
(test-eqv "ht-keys-len"   2  (vector-length (hashtable-keys h4)))
(test-eqv "ht-values-len" 2  (vector-length (hashtable-values h4)))

; hashtable-update!
(define h5 (make-eq-hashtable))
(hashtable-set! h5 'count 0)
(hashtable-update! h5 'count (lambda (v) (+ v 1)) 0)
(hashtable-update! h5 'count (lambda (v) (+ v 1)) 0)
(test-eqv "ht-update-incr" 2 (hashtable-ref h5 'count #f))

; hashtable-update! with missing key uses default
(hashtable-update! h5 'fresh (lambda (v) (* v 10)) 7)
(test-eqv "ht-update-default" 70 (hashtable-ref h5 'fresh #f))

; hashtable-walk
(define h6 (make-eq-hashtable))
(hashtable-set! h6 'a 1)
(hashtable-set! h6 'b 2)
(hashtable-set! h6 'c 3)
(define total 0)
(hashtable-walk h6 (lambda (k v) (set! total (+ total v))))
(test-eqv "ht-walk-sum" 6 total)
