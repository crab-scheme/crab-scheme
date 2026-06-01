; Conformance test for the (crab …) testing toolkit:
; expect (matchers) / mock / prop (property + fuzz) / spec (BDD runner).
; Uses the outer conformance prelude (test-equal/-true/-false) to assert
; on the toolkit's own behavior.

(test-section "(crab expect) — matchers")
(test-true "equal passes" (guard (e (#t #f)) (expect 5 (equal 5))))
(test-true "equal failure raises" (guard (e (#t #t)) (expect 5 (equal 6)) #f))
(test-true "be-true" (guard (e (#t #f)) (expect #t (be-true))))
(test-true "contain" (guard (e (#t #f)) (expect '(1 2 3) (contain 2))))
(test-true "have-len" (guard (e (#t #f)) (expect "abc" (have-len 3))))
(test-true "be->" (guard (e (#t #f)) (expect 5 (be-> 3))))
(test-true "be-close-to" (guard (e (#t #f)) (expect 3.14 (be-close-to 3.1 0.1))))
(test-true "be-empty" (guard (e (#t #f)) (expect '() (be-empty))))
(test-true "satisfy" (guard (e (#t #f)) (expect 4 (satisfy even?))))
(test-true "contain-substring" (guard (e (#t #f)) (expect "hello world" (contain-substring "lo w"))))
(test-true "expect-not inverts" (guard (e (#t #f)) (expect-not 5 (equal 6))))
(test-true "expect-raise catches a raise" (guard (e (#t #f)) (expect-raise (lambda () (error "boom")))))
(test-true "expect-raise fails when nothing raised"
           (guard (e (#t #t)) (expect-raise (lambda () 1)) #f))

(test-section "(crab mock)")
(define m (make-mock))
(mock-returns! m 42)
(test-equal "mock returns the configured value" 42 (m 1 2))
(test-equal "mock records the call" 1 (mock-call-count m))
(test-true "mock-called-with? literal" (mock-called-with? m 1 2))
(test-true "mock-called-with? (arg-any)" (mock-called-with? m 1 (arg-any)))
(test-true "mock-called-with? arg-that" (mock-called-with? m (arg-that odd?) 2))
(test-false "mock-called-with? mismatch" (mock-called-with? m 9 9))
(define m2 (make-mock))
(mock-returns-seq! m2 'a 'b 'c)
(test-equal "seq returns in order" '(a b c c) (list (m2) (m2) (m2) (m2)))
(define m3 (make-mock))
(mock-impl! m3 (lambda (x) (* x x)))
(test-equal "mock-impl computes from args" 16 (m3 4))
(define ma (make-mock)) (define mb (make-mock))
(ma) (mb)
(test-true "mock-called-before?" (mock-called-before? ma mb))
(test-false "mock-called-before? reversed" (mock-called-before? mb ma))

(test-section "(crab prop) — properties + fuzz")
(test-true "check passes a true property"
           (guard (e (#t #f)) (check (lambda (n) (= n (* 1 n))) (gen-int) 50)))
(test-true "check raises on a false property"
           (guard (e (#t #t)) (check (lambda (n) (< n 0)) (gen-int 0 100) 50) #f))
(test-true "gen-list-of yields lists"
           (guard (e (#t #f)) (check list? (gen-list-of (gen-int)) 20)))
(test-true "gen-one-of stays in set"
           (guard (e (#t #f)) (check (lambda (x) (member x '(a b c))) (gen-one-of 'a 'b 'c) 20)))
(test-true "fuzz with no crash passes"
           (guard (e (#t #f)) (fuzz string-length (gen-string) 20)))
(test-true "fuzz catches a crash"
           (guard (e (#t #t)) (fuzz car (gen-int) 20) #f))

(test-section "(crab spec) — BDD runner")
(spec:reset!)
(define *counter* 0)
(describe "math"
  (before-each (set! *counter* (+ *counter* 1)))
  (it "adds" (expect (+ 1 1) (equal 2)))
  (it "subtracts" (expect (- 3 1) (equal 2)))
  (context "nested"
    (it "multiplies" (expect (* 2 3) (equal 6)))))
(test-equal "all three specs pass" '(3 0 0 0) (run-specs))
(test-equal "before-each ran once per spec" 3 *counter*)

(spec:reset!)
(describe "mixed"
  (it "passes" (expect 1 (equal 1)))
  (it "fails" (expect 1 (equal 2)))
  (xit "pending" (expect 1 (equal 2))))
(test-equal "1 pass, 1 fail, 1 pending" '(1 1 1 0) (run-specs))

(spec:reset!)
(describe "focusing"
  (it "would-fail-but-skipped" (expect 1 (equal 2)))
  (fit "focused" (expect 1 (equal 1))))
(test-equal "focus runs only focused, skips the rest" '(1 0 0 1) (run-specs))

(test-section "(crab spec) — table-driven")
(spec:reset!)
(describe-table "addition"
  (lambda (a b s) (expect (+ a b) (equal s)))
  (entry "1+1=2" 1 1 2)
  (entry "2+3=5" 2 3 5)
  (entry "1+1=3 (bad)" 1 1 3))
(test-equal "table: 2 pass, 1 fail" '(2 1 0 0) (run-specs))
