(test-section "R7RS (load filename [environment])")

; --- load reads a file and evaluates each top-level expression ---
; Set up: write a test file that defines a helper, then load it.
(define tmpdir
  (or (get-environment-variable "TMPDIR") "/tmp"))

(define test-file (string-append tmpdir "/cs_load_test.scm"))

; Write a small Scheme file.
(define op (open-output-file test-file))
(write-string "(define loaded-value 42)" op)
(write-char #\newline op)
(write-string "(define (add-one x) (+ x 1))" op)
(close-output-port op)

; Load it.
(load test-file)

; --- definitions from the loaded file are now visible ---
(test-eqv "load-defines-value" 42 loaded-value)
(test-eqv "load-defines-fn"     6 (add-one 5))

; --- with explicit environment arg (ignored at foundation, must accept) ---
(define test-file2 (string-append tmpdir "/cs_load_test2.scm"))
(define op2 (open-output-file test-file2))
(write-string "(define loaded-value-2 'sentinel)" op2)
(close-output-port op2)
(load test-file2 (interaction-environment))
(test-eqv "load-with-env" 'sentinel loaded-value-2)

; --- non-existent file: caught as file-error ---
(define c1
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k (list 'caught (file-error? c))))
        (lambda () (load "/this/does/not/exist/abc.scm"))))))
(test-equal "load-bad-path" '(caught #t) c1)

; --- malformed scheme source: caught as read-error ---
(define test-file3 (string-append tmpdir "/cs_load_test3.scm"))
(define op3 (open-output-file test-file3))
(write-string "(define x 1" op3)  ; unterminated
(close-output-port op3)
(define c2
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k (list 'caught (read-error? c))))
        (lambda () (load test-file3))))))
(test-equal "load-bad-syntax" '(caught #t) c2)

; --- empty file is a no-op ---
(define test-file4 (string-append tmpdir "/cs_load_test4.scm"))
(define op4 (open-output-file test-file4))
(close-output-port op4)
(test-equal "load-empty-file" (if #f #f) (load test-file4))

; --- arity errors ---
(define c3
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (load))))))
(test-eqv "load-arity-0" 'caught c3)

; --- non-string filename caught ---
(define c4
  (call/cc
    (lambda (k)
      (with-exception-handler
        (lambda (c) (k 'caught))
        (lambda () (load 42))))))
(test-eqv "load-non-string" 'caught c4)
