(test-section "String extras + vector HO + read")

; trim
(test-equal "trim-both"   "hello"   (string-trim "  hello  "))
(test-equal "trim-left"   "hello  " (string-trim-left "  hello  "))
(test-equal "trim-right"  "  hello" (string-trim-right "  hello  "))
(test-equal "trim-empty"  ""        (string-trim "   "))
(test-equal "trim-clean"  "hello"   (string-trim "hello"))

; contains / index
(test-eqv "contains-found"     2  (string-contains "hello" "ll"))
(test-eqv "contains-start"     0  (string-contains "hello" "he"))
(test-eqv "contains-missing"   #f (string-contains "hello" "xyz"))
(test-eqv "index-char-found"   2  (string-index "hello" #\l))
(test-eqv "index-char-missing" #f (string-index "hello" #\z))

; split / join
(test-equal "split-comma"
  '("a" "b" "c")
  (string-split "a,b,c" ","))
(test-equal "split-space"
  '("hello" "world")
  (string-split "hello world" " "))
(test-equal "split-empty-input"  '("")  (string-split "" ","))
(test-equal "join-comma"
  "a,b,c"
  (string-join '("a" "b" "c") ","))
(test-equal "join-empty-sep"  "abc"  (string-join '("a" "b" "c")))

; reverse
(test-equal "reverse"  "olleh"  (string-reverse "hello"))
(test-equal "reverse-empty" "" (string-reverse ""))

; <-> vector
(test-equal "string->vector"
  (vector #\h #\i)
  (string->vector "hi"))
(test-equal "vector->string"
  "hi"
  (vector->string (vector #\h #\i)))

; vector-map
(test-equal "vector-map-square"
  #(1 4 9 16)
  (vector-map (lambda (x) (* x x)) #(1 2 3 4)))
(test-equal "vector-map-2-vecs"
  #(11 22 33)
  (vector-map + #(1 2 3) #(10 20 30)))

; vector-for-each
(define vfe-sum 0)
(vector-for-each (lambda (x) (set! vfe-sum (+ vfe-sum x))) #(1 2 3 4 5))
(test-eqv "vector-for-each-sum" 15 vfe-sum)

; read from string port
(define rp (open-string-input-port "(+ 1 2) 42 hello"))
(test-equal "read-list"   '(+ 1 2)   (read rp))
(test-eqv   "read-num"    42         (read rp))
(test-eqv   "read-sym"    'hello     (read rp))
(test-true  "read-eof"    (eof-object? (read rp)))
