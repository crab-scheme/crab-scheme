(test-section "let-values, dynamic-wind, with-*-string")

; let-values: single binding
(test-eqv "let-values-single"
  3
  (let-values (((a b) (values 1 2))) (+ a b)))

; let-values: multiple bindings
(test-eqv "let-values-multi"
  10
  (let-values (((a b) (values 1 2))
               ((c d) (values 3 4)))
    (+ a b c d)))

; let-values with one variable
(test-eqv "let-values-one-var"
  42
  (let-values (((x) (values 42))) x))

; let-values: empty bindings = body
(test-eqv "let-values-empty-bindings"
  99
  (let-values () 99))

; let*-values
(test-eqv "let*-values"
  6
  (let*-values (((a b) (values 1 2))
                ((c) (values (+ a b))))
    (+ a b c)))

; dynamic-wind: order
(define wind-log '())
(dynamic-wind
  (lambda () (set! wind-log (cons 'before wind-log)))
  (lambda () (set! wind-log (cons 'thunk wind-log)) 42)
  (lambda () (set! wind-log (cons 'after wind-log))))
(test-equal "dynamic-wind-order" '(before thunk after) (reverse wind-log))

; dynamic-wind returns thunk's value
(test-eqv "dynamic-wind-returns"
  100
  (dynamic-wind (lambda () #t) (lambda () 100) (lambda () #t)))

; dynamic-wind: after runs even on raise
(define after-ran #f)
(define result-from-handler
  (with-exception-handler
    (lambda (c) 'caught)
    (lambda ()
      (dynamic-wind
        (lambda () #t)
        (lambda () (raise 'boom))
        (lambda () (set! after-ran #t))))))
(test-true  "dynwind-after-on-raise" after-ran)
(test-eqv   "dynwind-raise-caught"   'caught result-from-handler)

; with-output-to-string
(test-equal "with-output-to-string"
  "hello world"
  (with-output-to-string (lambda () (display "hello ") (display "world"))))

; with-input-from-string + read-char (uses current-input-port? not yet wired
; for read-char without explicit port, but the helper installs the port)
(test-equal "with-input-installs-port"
  "yes"
  (with-input-from-string "ignored input" (lambda () "yes")))

; nested dynamic-wind
(define nest-log '())
(dynamic-wind
  (lambda () (set! nest-log (cons 'outer-before nest-log)))
  (lambda ()
    (dynamic-wind
      (lambda () (set! nest-log (cons 'inner-before nest-log)))
      (lambda () (set! nest-log (cons 'inner-thunk nest-log)))
      (lambda () (set! nest-log (cons 'inner-after nest-log)))))
  (lambda () (set! nest-log (cons 'outer-after nest-log))))
(test-equal "nested-dynamic-wind"
  '(outer-before inner-before inner-thunk inner-after outer-after)
  (reverse nest-log))
