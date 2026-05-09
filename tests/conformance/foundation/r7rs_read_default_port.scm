(test-section "R7RS read-char/peek-char/read-string with optional port (default current-input-port)")

; --- with-input-from-string sets current-input-port; calls without
;     port arg should use it ---
(test-equal "rc-no-port"
  '(#\a #\b #\c)
  (with-input-from-string "abc"
    (lambda ()
      (let ((c1 (read-char)) (c2 (read-char)) (c3 (read-char)))
        (list c1 c2 c3)))))

; --- peek-char without port ---
(test-equal "pc-no-port"
  '(#\x #\x)  ; peek doesn't consume — both peeks see same char
  (with-input-from-string "xy"
    (lambda ()
      (list (peek-char) (peek-char)))))

; --- peek then read returns same char ---
(test-equal "pc-then-rc"
  '(#\h #\h)
  (with-input-from-string "hello"
    (lambda ()
      (list (peek-char) (read-char)))))

; --- read-string without port ---
(test-equal "rs-no-port"
  "abc"
  (with-input-from-string "abcdef"
    (lambda () (read-string 3))))

; --- read-string EOF returns eof object ---
(test-true "rs-eof"
  (with-input-from-string ""
    (lambda () (eof-object? (read-string 5)))))

; --- read-char EOF ---
(test-true "rc-eof"
  (with-input-from-string ""
    (lambda () (eof-object? (read-char)))))

; --- explicit port still works ---
(define p (open-input-string "abc"))
(test-equal "rc-explicit-port" #\a (read-char p))
(test-equal "rc-explicit-port-2" #\b (read-char p))
(test-equal "pc-explicit-port" #\c (peek-char p))
(test-equal "rc-explicit-port-3" #\c (read-char p))
(test-true  "rc-explicit-eof"   (eof-object? (read-char p)))

; --- read-string with explicit port ---
(define p2 (open-input-string "world"))
(test-equal "rs-explicit-port" "wor" (read-string 3 p2))
(test-equal "rs-explicit-rest" "ld" (read-string 99 p2))
(test-true  "rs-explicit-eof" (eof-object? (read-string 99 p2)))

; --- nested with-input-from-string restores context ---
(define outer-port (open-input-string "OUTER"))
(define result
  (parameterize ()
    (with-input-from-string "INNER"
      (lambda ()
        (read-char)))))  ; reads from "INNER" not "OUTER"
(test-equal "nested-input-1" #\I result)

; --- read-char eats one char per call ---
(test-equal "rc-stream"
  '(#\1 #\2 #\3 #\4)
  (with-input-from-string "1234"
    (lambda ()
      (let* ((a (read-char))
             (b (read-char))
             (c (read-char))
             (d (read-char)))
        (list a b c d)))))

; --- arity errors ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (read-char 1 2 3))))))
(test-eqv "rc-too-many-args" 'caught c1)

(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (read-string))))))
(test-eqv "rs-needs-k" 'caught c2)
