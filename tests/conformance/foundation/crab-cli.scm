; Conformance test for `(crab cli)` — argument & flag parsing.

; Naive substring search built from R6RS base procedures only, so
; the usage-text assertions don't depend on SRFI-13 / R7RS-large.
(define (str-contains? hay needle)
  (let ((hn (string-length hay)) (nn (string-length needle)))
    (let loop ((i 0))
      (cond ((> (+ i nn) hn) #f)
            ((string=? (substring hay i (+ i nn)) needle) #t)
            (else (loop (+ i 1)))))))

(test-section "(crab cli) — descriptors")

(define verbose-opt (cli-option "verbose" "v" "flag" #f "be loud"))
(define name-opt (cli-option "name" #f "string" "world" "who to greet"))
(define count-opt (cli-option "count" "n" "int" 1 "repeat count"))
(define ratio-opt (cli-option "ratio" #f "float" 1.0 "a ratio"))
(define opts (list verbose-opt name-opt count-opt ratio-opt))

(test-true "cli-option? recognizes a descriptor" (cli-option? verbose-opt))
(test-false "cli-option? rejects a string" (cli-option? "verbose"))
(test-false "cli-option? rejects a plain vector" (cli-option? (vector 1 2 3)))

(test-section "(crab cli) — defaults when absent")

(define empty-result (cli-parse opts '()))
(test-false "flag default is #f" (cdr (assoc "verbose" empty-result)))
(test-equal "string default preserved" "world" (cdr (assoc "name" empty-result)))
(test-equal "int default preserved" 1 (cdr (assoc "count" empty-result)))
(test-equal "positionals default empty" '() (cdr (assoc "--" empty-result)))

(test-section "(crab cli) — long options")

(define r1 (cli-parse opts (list "--verbose" "--name=Ada" "--count" "3")))
(test-true "--verbose sets the flag" (cdr (assoc "verbose" r1)))
(test-equal "--name=Ada inline value" "Ada" (cdr (assoc "name" r1)))
(test-equal "--count 3 separate value" 3 (cdr (assoc "count" r1)))

(test-section "(crab cli) — short options")

(define r2 (cli-parse opts (list "-v" "-n" "7")))
(test-true "-v sets the flag" (cdr (assoc "verbose" r2)))
(test-equal "-n 7 short with value" 7 (cdr (assoc "count" r2)))

(define r3 (cli-parse opts (list "-n=9")))
(test-equal "-n=9 short inline value" 9 (cdr (assoc "count" r3)))

(test-section "(crab cli) — typed values")

(define r4 (cli-parse opts (list "--ratio" "2.5")))
(test-equal "--ratio parses a float" 2.5 (cdr (assoc "ratio" r4)))

(define r5 (cli-parse opts (list "--count" "-5")))
(test-equal "negative int as next token" -5 (cdr (assoc "count" r5)))

(test-section "(crab cli) — positionals & terminator")

(define r6 (cli-parse opts (list "a" "--verbose" "b" "c")))
(test-equal "positionals collected in order" '("a" "b" "c") (cdr (assoc "--" r6)))
(test-true "options still parsed amid positionals" (cdr (assoc "verbose" r6)))

(define r7 (cli-parse opts (list "--" "--verbose" "-n")))
(test-false "-- stops option parsing" (cdr (assoc "verbose" r7)))
(test-equal "tokens after -- are positional" '("--verbose" "-n") (cdr (assoc "--" r7)))

(define r8 (cli-parse opts (list "-")))
(test-equal "lone - is a positional" '("-") (cdr (assoc "--" r8)))

(test-section "(crab cli) — usage text")

(define usage (cli-usage "greet" "Greets someone." opts))
(test-true "usage mentions program name" (str-contains? usage "greet"))
(test-true "usage has a Usage: line" (str-contains? usage "Usage:"))
(test-true "usage lists --name" (str-contains? usage "--name"))
(test-true "usage shows the short flag" (str-contains? usage "-v"))

(test-section "(crab cli) — errors")

(test-true "unknown option raises"
           (guard (e (#t #t))
             (cli-parse opts (list "--nope"))
             #f))

(test-true "missing value raises"
           (guard (e (#t #t))
             (cli-parse opts (list "--name"))
             #f))

(test-true "non-integer for int raises"
           (guard (e (#t #t))
             (cli-parse opts (list "--count" "abc"))
             #f))

(test-true "flag given a value raises"
           (guard (e (#t #t))
             (cli-parse opts (list "--verbose=yes"))
             #f))
