; Conformance test for `(crab process)` — stdlib-modules iter 3.

(test-section "(crab process) — which")

; /bin/sh exists on every unix-like CI; if not we'd need to special-case.
; The user can rely on this being present on macos and linux runners.
(test-true "which sh returns a string"
           (let ((result (which "sh")))
             (and (string? result) (> (string-length result) 0))))

(test-false "which on a nonsense command returns #f"
            (which "__crab_process_definitely_not_a_real_command__"))

(test-section "(crab process) — run")

; echo "hello, crab" through /bin/sh -c
(define __echo-result__ (run "sh" (list "-c" "echo hello, crab")))
(test-eqv "run echo exit code 0"
          0
          (car __echo-result__))
(test-equal "run echo stdout"
            "hello, crab\n"
            (car (cdr __echo-result__)))
(test-equal "run echo stderr empty"
            ""
            (car (cdr (cdr __echo-result__))))

; non-zero exit
(define __false-result__ (run "sh" (list "-c" "exit 7")))
(test-eqv "run exit 7 reports exit code 7"
          7
          (car __false-result__))

; stderr capture
(define __err-result__ (run "sh" (list "-c" "echo whoops 1>&2; exit 0")))
(test-equal "run captures stderr"
            "whoops\n"
            (car (cdr (cdr __err-result__))))

; stdin payload — sh -c cat reads stdin and echoes to stdout
(define __cat-result__ (run "sh" (list "-c" "cat") "piped input\n"))
(test-equal "run forwards stdin to child"
            "piped input\n"
            (car (cdr __cat-result__)))

(test-section "(crab process) — run/status")

(test-eqv "run/status true returns 0" 0 (run/status "sh" (list "-c" "exit 0")))
(test-eqv "run/status exit 3 returns 3" 3 (run/status "sh" (list "-c" "exit 3")))
