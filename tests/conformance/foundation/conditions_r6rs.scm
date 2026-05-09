(test-section "R6RS condition types: simple, compound, hierarchy")

; --- simple constructors + predicates ---
(define m (make-message-condition "boom"))
(test-true  "message-cond-cond?"     (condition? m))
(test-true  "message-cond-pred"      (message-condition? m))
(test-equal "message-cond-accessor"  "boom" (condition-message m))
(test-false "message-cond-not-error" (error? m))

(define i (make-irritants-condition '(1 2 3)))
(test-true  "irritants-cond-pred"    (irritants-condition? i))
(test-equal "irritants-cond-accessor" '(1 2 3) (condition-irritants i))

(define w (make-warning))
(test-true  "warning-pred"           (warning? w))
(test-false "warning-not-serious"    (serious-condition? w))

(define s (make-serious-condition))
(test-true  "serious-pred"           (serious-condition? s))
(test-false "serious-not-warning"    (warning? s))

(define e (make-error))
(test-true  "error-pred"             (error? e))
(test-true  "error-is-serious"       (serious-condition? e))
(test-false "error-not-violation"    (violation? e))

(define v (make-violation))
(test-true  "viol-pred"              (violation? v))
(test-true  "viol-is-serious"        (serious-condition? v))
(test-false "viol-not-error"         (error? v))

(define av (make-assertion-violation))
(test-true  "av-pred"                (assertion-violation? av))
(test-true  "av-is-violation"        (violation? av))
(test-true  "av-is-serious"          (serious-condition? av))

(define ncv (make-non-continuable-violation))
(test-true  "ncv-pred"               (non-continuable-violation? ncv))
(test-true  "ncv-is-violation"       (violation? ncv))

(define wh (make-who-condition 'caller))
(test-true  "who-pred"               (who-condition? wh))
(test-equal "who-accessor"           'caller (condition-who wh))

; --- compound conditions ---
(define c (condition (make-error) (make-message-condition "bad") (make-irritants-condition '(x y))))
(test-true  "compound-cond?"         (condition? c))
(test-true  "compound-error?"        (error? c))
(test-true  "compound-msg?"          (message-condition? c))
(test-true  "compound-irritants?"    (irritants-condition? c))
(test-equal "compound-message"       "bad" (condition-message c))
(test-equal "compound-irritants"     '(x y) (condition-irritants c))

; condition flattens nested compounds
(define c2 (condition c (make-who-condition 'sub)))
(test-true  "nested-cond?"           (condition? c2))
(test-true  "nested-error?"          (error? c2))
(test-equal "nested-who"             'sub (condition-who c2))
(test-equal "nested-message"         "bad" (condition-message c2))

; simple-conditions returns one element per simple
(test-eqv   "simple-conds-len-3"     3 (length (simple-conditions c)))
(test-eqv   "simple-conds-len-4"     4 (length (simple-conditions c2)))

; --- non-conditions ---
(test-false "cond-num"               (condition? 42))
(test-false "cond-list"              (condition? '(1 2 3)))
(test-false "cond-vector-untagged"   (condition? #(a b c)))
(test-false "cond-string"            (condition? "hi"))
(test-false "msg-on-num"             (message-condition? 42))
(test-false "error-on-num"           (error? 42))

; --- error builtin produces a proper compound ---
(define caught
  (with-exception-handler (lambda (c) c) (lambda () (error "bang" 1 2))))
(test-true  "raised-cond?"           (condition? caught))
(test-true  "raised-error?"          (error? caught))
(test-true  "raised-msg?"            (message-condition? caught))
(test-true  "raised-irritants?"      (irritants-condition? caught))
(test-equal "raised-message"         "bang" (condition-message caught))
(test-equal "raised-irritants"       '(1 2) (condition-irritants caught))
(test-true  "raised-error-object?"   (error-object? caught))
(test-equal "raised-eom"             "bang" (error-object-message caught))
(test-equal "raised-eoi"             '(1 2) (error-object-irritants caught))

; raised condition without irritants
(define caught2
  (with-exception-handler (lambda (c) c) (lambda () (error "no irritants"))))
(test-equal "no-irritants-msg"       "no irritants" (condition-message caught2))
(test-false "no-irritants-cond?"     (irritants-condition? caught2))

; bare simple (no compound wrapper applied externally) is still recognised
; — make-message-condition returns a one-element compound, and condition-*
; predicates/accessors work transparently.
(test-true  "bare-msg-cond?"         (condition? (make-message-condition "x")))
(test-true  "bare-msg-msg-cond?"     (message-condition? (make-message-condition "x")))
