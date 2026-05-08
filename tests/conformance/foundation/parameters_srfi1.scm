(test-section "Parameters + parameterize + SRFI-1 extras")

; Parameters
(define p (make-parameter 10))
(test-eqv "param-initial"   10  (p))

; Setting via direct call
(p 99)
(test-eqv "param-after-set" 99  (p))
(p 10) ; restore

; parameterize: temporarily binds, restores after
(test-eqv "parameterize-inner"
  42
  (parameterize ((p 42)) (p)))
(test-eqv "param-restored-after-parameterize" 10 (p))

; Multiple parameters in one parameterize
(define p2 (make-parameter "a"))
(test-equal "parameterize-multi"
  (list 1 "z")
  (parameterize ((p 1) (p2 "z"))
    (list (p) (p2))))
(test-eqv   "param-p-restored"  10  (p))
(test-equal "param-p2-restored" "a" (p2))

; parameterize unwinds before handler runs (dynamic-wind after-thunk runs
; first on non-local exit, so handler sees the restored value)
(define caught-before #f)
(define caught-after #f)
(with-exception-handler
  (lambda (c) (set! caught-after (p)))
  (lambda ()
    (parameterize ((p 999))
      (set! caught-before (p))
      (raise 'boom))))
(test-eqv "param-inside-parameterize" 999 caught-before)
(test-eqv "param-handler-sees-restored" 10 caught-after)
(test-eqv "param-restored-after-raise" 10 (p))

; SRFI-1: delete
(test-equal "delete-2"
  '(1 3 4)
  (delete 2 '(1 2 3 2 4)))
(test-equal "delete-not-present"
  '(1 2 3)
  (delete 99 '(1 2 3)))
(test-equal "delete-empty"  '()  (delete 1 '()))

; delete-duplicates
(test-equal "ddup-mixed"
  '(1 2 3 4)
  (delete-duplicates '(1 2 3 2 1 4 3)))
(test-equal "ddup-empty" '() (delete-duplicates '()))
(test-equal "ddup-already-unique" '(a b c) (delete-duplicates '(a b c)))

; concatenate
(test-equal "concatenate"
  '(1 2 3 4 5 6)
  (concatenate '((1 2) (3) (4 5 6))))
(test-equal "concatenate-empty-inner"
  '(1 2)
  (concatenate '(() (1 2) ())))
(test-equal "concatenate-of-empty" '() (concatenate '()))

; first / second / third
(test-eqv "first-3"  1 (first '(1 2 3 4)))
(test-eqv "second-3" 2 (second '(1 2 3 4)))
(test-eqv "third-3"  3 (third '(1 2 3 4)))

; tabulate
(test-equal "tabulate-squares"
  '(0 1 4 9 16)
  (tabulate 5 (lambda (i) (* i i))))
(test-equal "tabulate-zero" '() (tabulate 0 (lambda (i) i)))

; remove (inverse of filter)
(test-equal "remove-odd"
  '(2 4)
  (remove odd? '(1 2 3 4 5)))
(test-equal "remove-all-no-match"
  '(1 2 3)
  (remove (lambda (x) #f) '(1 2 3)))

; hashtable->alist / alist->hashtable
(define h (make-eq-hashtable))
(hashtable-set! h 'a 1)
(hashtable-set! h 'b 2)
(test-eqv "ht-to-alist-len"
  2
  (length (hashtable->alist h)))

(define h2 (alist->hashtable '((x . 1) (y . 2) (z . 3))))
(test-eqv "alist-to-ht-size"  3  (hashtable-size h2))
(test-eqv "alist-to-ht-y"     2  (hashtable-ref h2 'y #f))
