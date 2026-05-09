(test-section "R7RS call-with-port, call-with-input-string, call-with-output-string")

; --- call-with-input-string: reads chars from a string port ---
(test-equal "cwis-readall"
  '(#\h #\i)
  (call-with-input-string "hi"
    (lambda (p)
      (let loop ((acc '()))
        (let ((c (read-char p)))
          (if (eof-object? c)
              (reverse acc)
              (loop (cons c acc))))))))

; --- call-with-input-string: read whole datum ---
(test-equal "cwis-read-datum" '(+ 1 2 3)
  (call-with-input-string "(+ 1 2 3)" read))

(test-eqv "cwis-read-num" 42
  (call-with-input-string "42" read))

; --- call-with-output-string: collects writes ---
(test-equal "cwos-write"
  "hello world"
  (call-with-output-string
    (lambda (p)
      (write-string "hello " p)
      (write-string "world" p))))

(test-equal "cwos-empty"
  ""
  (call-with-output-string (lambda (p) p)))

; --- call-with-output-string returns proc value's output, not its return value ---
(test-equal "cwos-only-output"
  "abc"
  (call-with-output-string
    (lambda (p)
      (display "abc" p)
      'ignored-return-value)))

; --- call-with-port works with both input and output ports ---
(define out-port (open-output-string))
(test-equal "cwp-output"
  ; The proc returns the string read from a separate port,
  ; verifying call-with-port passes the port and returns proc's value.
  "captured"
  (call-with-port out-port
    (lambda (p)
      (write-string "captured" p)
      (get-output-string p))))

; --- call-with-port input-side ---
(test-eqv "cwp-input" 7
  (call-with-port (open-input-string "7 8")
    (lambda (p) (read p))))

; --- call-with-port returns the proc's return value (not close result) ---
(test-equal "cwp-returns-proc-value"
  '(1 2 3)
  (call-with-port (open-input-string "(1 2 3)")
    (lambda (p) (read p))))

; --- nested call-with-output-string ---
(test-equal "cwos-nested"
  "outer-inner"
  (call-with-output-string
    (lambda (p)
      (write-string "outer-" p)
      (write-string
        (call-with-output-string
          (lambda (q) (write-string "inner" q)))
        p))))

; --- call-with-input-string passes port, not string ---
(test-true "cwis-port-arg"
  (call-with-input-string "x"
    (lambda (p) (port? p))))

; --- call-with-output-string passes a writable port ---
(test-true "cwos-port-arg"
  (let ((seen-port #f))
    (call-with-output-string
      (lambda (p)
        (set! seen-port p)))
    (port? seen-port)))
