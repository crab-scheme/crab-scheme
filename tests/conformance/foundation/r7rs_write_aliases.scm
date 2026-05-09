(test-section "R7RS write-simple / write-shared aliases")

; --- write-simple writes the same as write (no shared notation) ---
(test-equal "ws-int"
  "42"
  (with-output-to-string (lambda () (write-simple 42))))

(test-equal "ws-string"
  "\"hello\""
  (with-output-to-string (lambda () (write-simple "hello"))))

(test-equal "ws-symbol"
  "foo"
  (with-output-to-string (lambda () (write-simple 'foo))))

(test-equal "ws-list"
  "(1 2 3)"
  (with-output-to-string (lambda () (write-simple '(1 2 3)))))

(test-equal "ws-nested"
  "(a (b c) d)"
  (with-output-to-string (lambda () (write-simple '(a (b c) d)))))

; --- write-shared also writes (foundation: same as write) ---
(test-equal "wsh-int"
  "100"
  (with-output-to-string (lambda () (write-shared 100))))

(test-equal "wsh-string"
  "\"world\""
  (with-output-to-string (lambda () (write-shared "world"))))

(test-equal "wsh-list"
  "(x y z)"
  (with-output-to-string (lambda () (write-shared '(x y z)))))

; --- explicit port forms ---
(define op1 (open-output-string))
(write-simple 'hello op1)
(test-equal "ws-explicit-port" "hello" (get-output-string op1))

(define op2 (open-output-string))
(write-shared "foo" op2)
(test-equal "wsh-explicit-port" "\"foo\"" (get-output-string op2))

; --- chars use #\ notation ---
(test-equal "ws-char"
  "#\\A"
  (with-output-to-string (lambda () (write-simple #\A))))

; --- booleans ---
(test-equal "ws-true"
  "#t"
  (with-output-to-string (lambda () (write-simple #t))))

(test-equal "ws-false"
  "#f"
  (with-output-to-string (lambda () (write-simple #f))))
