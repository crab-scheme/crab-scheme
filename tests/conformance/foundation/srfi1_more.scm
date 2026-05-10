(test-section "SRFI-1 more: list selectors, predicates, ops")

(define ten '(1 2 3 4 5 6 7 8 9 10))

;; ---- selectors fourth..tenth ----
(test-equal "fourth"   4  (fourth  ten))
(test-equal "fifth"    5  (fifth   ten))
(test-equal "sixth"    6  (sixth   ten))
(test-equal "seventh"  7  (seventh ten))
(test-equal "eighth"   8  (eighth  ten))
(test-equal "ninth"    9  (ninth   ten))
(test-equal "tenth"   10  (tenth   ten))

(test-true "fourth-rejects-short"
  (guard (c (#t #t))
    (fourth '(1 2))
    #f))

;; ---- type predicates ----
(test-true  "not-pair?-fixnum"      (not-pair? 5))
(test-false "not-pair?-pair"        (not-pair? (cons 1 2)))
(test-true  "not-pair?-null"        (not-pair? '()))

(test-true  "null-list?-empty"      (null-list? '()))
(test-false "null-list?-cons"       (null-list? '(1)))

(test-true  "proper-list?-empty"    (proper-list? '()))
(test-true  "proper-list?-1234"     (proper-list? '(1 2 3 4)))
(test-false "proper-list?-improper" (proper-list? (cons 1 2)))
(test-false "proper-list?-num"      (proper-list? 5))

(test-true  "dotted-list?-cons-12"  (dotted-list? (cons 1 2)))
(test-false "dotted-list?-list-12"  (dotted-list? '(1 2)))
(test-false "dotted-list?-null"     (dotted-list? '()))

(test-false "circular-list?-proper" (circular-list? '(1 2 3)))

;; --- circular detection via tortoise/hare on a constructed cycle ---
(let ((p (list 1 2 3)))
  (set-cdr! (cddr p) p)            ; close the loop
  (test-true  "circular-list?-cycle" (circular-list? p))
  (test-false "proper-list?-cycle"   (proper-list? p))
  (test-false "dotted-list?-cycle"   (dotted-list? p)))

;; ---- append-reverse ----
(test-equal "append-reverse"        '(1 2 3 4 5 6) (append-reverse '(3 2 1) '(4 5 6)))
(test-equal "append-reverse-empty"  '(a b c)       (append-reverse '() '(a b c)))
(test-equal "append-reverse-empty2" '(3 2 1)       (append-reverse '(1 2 3) '()))

;; ---- reverse! ----
(test-equal "reverse!"              '(4 3 2 1)     (reverse! '(1 2 3 4)))
(test-equal "reverse!-empty"        '()            (reverse! '()))

;; ---- split-at ----
(call-with-values (lambda () (split-at '(1 2 3 4 5) 2))
  (lambda (a b)
    (test-equal "split-at-head" '(1 2)     a)
    (test-equal "split-at-tail" '(3 4 5)   b)))

(call-with-values (lambda () (split-at '(a b c) 0))
  (lambda (a b)
    (test-equal "split-at-0-head" '()      a)
    (test-equal "split-at-0-tail" '(a b c) b)))

(call-with-values (lambda () (split-at '(x y z) 3))
  (lambda (a b)
    (test-equal "split-at-end-head" '(x y z) a)
    (test-equal "split-at-end-tail" '()      b)))

(test-true "split-at-rejects-overflow"
  (guard (c (#t #t))
    (split-at '(1) 5)
    #f))
