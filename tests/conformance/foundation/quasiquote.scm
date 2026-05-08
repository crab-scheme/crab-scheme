(test-section "quasiquote / unquote / unquote-splicing")

; Plain quasiquote with no unquotes is just like quote
(test-equal "qq-plain"     '(a b c)   `(a b c))
(test-equal "qq-empty"     '()        `())

; Unquote: evaluates the form
(test-equal "unquote-num"
  '(1 2 3)
  `(1 ,(+ 1 1) 3))

; Multiple unquotes
(test-equal "unquote-multi"
  '(a 10 b 20 c 30)
  (let ((x 10) (y 20) (z 30)) `(a ,x b ,y c ,z)))

; unquote-splicing: splices a list inline
(test-equal "splice-inline"
  '(1 2 3 4 5)
  (let ((xs '(2 3 4))) `(1 ,@xs 5)))

(test-equal "splice-empty"
  '(1 2)
  (let ((xs '())) `(1 ,@xs 2)))

(test-equal "splice-only"
  '(a b c)
  (let ((xs '(a b c))) `(,@xs)))

; Mixed: unquote and unquote-splicing in same template
(test-equal "qq-mixed"
  '(start 0 1 2 mid 99 end)
  (let ((xs '(0 1 2)) (m 99))
    `(start ,@xs mid ,m end)))

; Quasiquote inside a vector
(test-equal "qq-vector"
  #(1 5 4)
  `#(1 ,(+ 2 3) 4))

(test-equal "qq-vector-multiple"
  #(a 10 b 20)
  (let ((x 10) (y 20)) `#(a ,x b ,y)))

; Nested quasiquote: inner quasiquote increments depth
(test-equal "qq-nested-1"
  '(a `(b ,(+ 1 2)) c)
  `(a `(b ,(+ 1 2)) c))

; At outer depth 1, the inner unquote stays unevaluated
; (it'll only fire at the inner level)

; Empty unquote-splicing of nothing
(test-equal "splice-with-extras"
  '(prefix x y z suffix)
  `(prefix ,@'(x y z) suffix))

; Quasiquote with computed expressions
(define (mk-greeting name)
  `(hello ,name how are you))
(test-equal "qq-in-fn"
  '(hello world how are you)
  (mk-greeting 'world))
