(test-section "copy variants + unfold + hashtable-fold")

; vector-copy
(test-equal "vec-copy-full"
  #(1 2 3)
  (vector-copy #(1 2 3)))
(test-equal "vec-copy-range"
  #(2 3)
  (vector-copy #(1 2 3 4) 1 3))
(test-equal "vec-copy-empty-range"
  #()
  (vector-copy #(1 2 3) 1 1))

; vector-copy! mutates in place
(define vc-dest (vector 0 0 0 0 0))
(vector-copy! vc-dest 1 #(7 8 9))
(test-equal "vec-copy!-mutates" #(0 7 8 9 0) vc-dest)

(define vc-dest2 (vector 0 0 0 0 0))
(vector-copy! vc-dest2 0 #(1 2 3 4 5) 1 4)
(test-equal "vec-copy!-with-range" #(2 3 4 0 0) vc-dest2)

; bytevector-copy!
(define bv-dest (make-bytevector 5 0))
(bytevector-copy! bv-dest 1 (bytevector 10 20 30))
(test-equal "bv-copy!-mutates"
  '(0 10 20 30 0)
  (bytevector->u8-list bv-dest))

; bytevector-nul-unescape (cw-71k): 0x00 0xFF -> 0x00, terminated by 0x00 0x00
(test-equal "bv-nul-unescape-no-escapes"
  '(1 2 3)
  (bytevector->u8-list (bytevector-nul-unescape (bytevector 1 2 3 0 0) 0)))
(test-equal "bv-nul-unescape-one-escape"
  '(1 0 2)
  (bytevector->u8-list (bytevector-nul-unescape (bytevector 1 0 255 2 0 0) 0)))
(test-equal "bv-nul-unescape-leading-run"
  '(0 0 1)
  (bytevector->u8-list (bytevector-nul-unescape (bytevector 0 255 0 255 1 0 0) 0)))
(test-equal "bv-nul-unescape-empty"
  '()
  (bytevector->u8-list (bytevector-nul-unescape (bytevector 0 0) 0)))
(test-equal "bv-nul-unescape-start-offset"
  '(9 8)
  (bytevector->u8-list (bytevector-nul-unescape (bytevector 1 1 9 8 0 0) 2)))

; string-copy!
(define s-dest (make-string 5 #\.))
(string-copy! s-dest 1 "abc")
(test-equal "string-copy!-mutates" ".abc." s-dest)

(define s-dest2 (make-string 5 #\.))
(string-copy! s-dest2 0 "xyz!" 0 3)
(test-equal "string-copy!-with-range" "xyz.." s-dest2)

; unfold (squares 0..5)
(test-equal "unfold-squares"
  '(0 1 4 9 16 25)
  (unfold (lambda (n) (> n 5))
          (lambda (n) (* n n))
          (lambda (n) (+ n 1))
          0))

; unfold producing empty
(test-equal "unfold-empty"
  '()
  (unfold (lambda (n) #t) (lambda (n) n) (lambda (n) (+ n 1)) 0))

; zip-with (alias for map)
(test-equal "zip-with-add"
  '(11 22 33)
  (zip-with + '(1 2 3) '(10 20 30)))

; hashtable-fold sums values
(define h (make-eq-hashtable))
(hashtable-set! h 'a 1)
(hashtable-set! h 'b 2)
(hashtable-set! h 'c 3)
(test-eqv "ht-fold-sum"
  6
  (hashtable-fold (lambda (k v acc) (+ v acc)) 0 h))

; hashtable-for-each: collect keys
(define collected '())
(hashtable-for-each (lambda (k v) (set! collected (cons k collected))) h)
(test-eqv "ht-for-each-len" 3 (length collected))

; error? recognises raised conditions; assertion-violation? does NOT
; (R6RS distinguishes — `error` produces &error, not &assertion).
(test-true "error-of-error"
  (with-exception-handler
    (lambda (c) (error? c))
    (lambda () (error "oops"))))
(test-false "assertion-violation-of-error"
  (with-exception-handler
    (lambda (c) (assertion-violation? c))
    (lambda () (error "oops"))))
(test-false "assertion-violation-of-num" (assertion-violation? 42))
(test-false "error-of-num" (error? 42))
