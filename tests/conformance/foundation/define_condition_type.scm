(test-section "define-condition-type")

; Single-level user type extending &error.
(define-condition-type &http-error &error make-http-error http-error?
  (status http-error-status))

(define he (make-http-error 404))
(test-true  "http-error-cond?"       (condition? he))
(test-true  "http-error-pred"        (http-error? he))
(test-eqv   "http-error-status"      404 (http-error-status he))
; Inherits from &error — standard predicate matches.
(test-true  "http-error-is-error"    (error? he))
(test-true  "http-error-is-serious"  (serious-condition? he))
; But it's NOT a violation/assertion/warning/etc.
(test-false "http-error-not-violation" (violation? he))
(test-false "http-error-not-warning" (warning? he))
(test-false "http-error-not-msg"     (message-condition? he))

; Compound: combine user type with standard simples.
(define ec (condition he (make-message-condition "not found")))
(test-true  "compound-http-error"    (http-error? ec))
(test-true  "compound-error"         (error? ec))
(test-true  "compound-msg"           (message-condition? ec))
(test-equal "compound-msg-text"      "not found" (condition-message ec))
(test-eqv   "compound-status"        404 (http-error-status ec))

; Multi-field user type.
(define-condition-type &range-error &error
  make-range-error range-error?
  (lo range-error-lo)
  (hi range-error-hi)
  (val range-error-val))

(define re (make-range-error 0 100 250))
(test-eqv "range-lo"  0   (range-error-lo re))
(test-eqv "range-hi"  100 (range-error-hi re))
(test-eqv "range-val" 250 (range-error-val re))
(test-true "range-is-error" (error? re))
(test-false "range-not-http" (http-error? re))

; Three-level chain: &not-found inherits &http-error inherits &error.
(define-condition-type &not-found &http-error
  make-not-found not-found?
  (path not-found-path))

(define nf (make-not-found 410 "/gone"))
(test-eqv   "not-found-status"     410 (http-error-status nf))
(test-equal "not-found-path"       "/gone" (not-found-path nf))
(test-true  "not-found-pred"       (not-found? nf))
(test-true  "not-found-is-http"    (http-error? nf))
(test-true  "not-found-is-error"   (error? nf))

; A bare &http-error is not a &not-found.
(test-false "http-not-not-found"   (not-found? he))

; User type with no fields.
(define-condition-type &timeout &error make-timeout timeout?)
(define to (make-timeout))
(test-true "timeout-pred"          (timeout? to))
(test-true "timeout-is-error"      (error? to))

; Predicate negative: non-conditions return #f.
(test-false "http-on-num"          (http-error? 42))
(test-false "http-on-vec"          (http-error? #(a b c)))
(test-false "http-on-list"         (http-error? '(1 2)))
