(test-section "case form + assoc/member family")

; case
(define (kind n)
  (case n
    ((1 2 3) 'small)
    ((10 20 30) 'medium)
    ((100 200 300) 'large)
    (else 'unknown)))
(test-eqv "case-small-1"   'small   (kind 1))
(test-eqv "case-small-3"   'small   (kind 3))
(test-eqv "case-medium-20" 'medium  (kind 20))
(test-eqv "case-large-100" 'large   (kind 100))
(test-eqv "case-unknown"   'unknown (kind 999))

; case with symbols
(define (color-num c)
  (case c
    ((red) 1)
    ((green blue) 2)
    (else 0)))
(test-eqv "case-symbol-red"   1  (color-num 'red))
(test-eqv "case-symbol-green" 2  (color-num 'green))
(test-eqv "case-symbol-blue"  2  (color-num 'blue))
(test-eqv "case-symbol-other" 0  (color-num 'yellow))

; assoc / assv / assq
(define al '((1 . a) (2 . b) (3 . c)))
(test-equal "assoc-found"     '(2 . b)  (assoc 2 al))
(test-eqv   "assoc-missing"   #f        (assoc 99 al))
(test-equal "assv-found"      '(1 . a)  (assv 1 al))
(test-eqv   "assv-missing"    #f        (assv 99 al))

(define al-sym '((red . 1) (green . 2)))
(test-equal "assq-sym-red"    '(red . 1)    (assq 'red al-sym))
(test-equal "assq-sym-green"  '(green . 2)  (assq 'green al-sym))
(test-eqv   "assq-sym-miss"   #f            (assq 'blue al-sym))

; member / memv / memq
(test-equal "member-found"   '(3 4 5)  (member 3 '(1 2 3 4 5)))
(test-eqv   "member-missing" #f        (member 99 '(1 2 3)))
(test-equal "memv-found"     '(b c)    (memv 'b '(a b c)))
(test-equal "memq-found"     '(b c)    (memq 'b '(a b c)))
