(test-section "let-syntax + nested patterns + advanced macros")

; let-syntax: local macro
(test-eqv "let-syntax-basic"
  42
  (let-syntax ((my-double (syntax-rules () ((_ x) (* x 2)))))
    (my-double 21)))

; let-syntax: macro doesn't escape its scope
(test-eqv "let-syntax-scoped"
  100
  (begin
    (let-syntax ((local-mac (syntax-rules () ((_ x) (+ x 1)))))
      (local-mac 99))
    100))   ; if local-mac leaked, the 100 here would be (+ 100 1) = 101

; letrec-syntax: same as let-syntax for our impl
(test-eqv "letrec-syntax-basic"
  9
  (letrec-syntax ((triple (syntax-rules () ((_ x) (* x 3)))))
    (triple (+ 1 2))))

; Nested patterns: ((k v) ...) matches list of pairs
(define-syntax pairs->list
  (syntax-rules ()
    ((_ (k v) ...) (list (list (quote k) v) ...))))
(test-equal "nested-pattern-pairs"
  '((a 1) (b 2) (c 3))
  (pairs->list (a 1) (b 2) (c 3)))
(test-equal "nested-pattern-empty"
  '()
  (pairs->list))

; Self-defined `let` via macro
(define-syntax my-let
  (syntax-rules ()
    ((_ ((name val) ...) body ...)
     ((lambda (name ...) body ...) val ...))))
(test-eqv "my-let-binding"
  30
  (my-let ((x 10) (y 20)) (+ x y)))
(test-eqv "my-let-shadow"
  5
  (my-let ((x 5)) x))
(test-eqv "my-let-empty"
  42
  (my-let () 42))

; Recursive macro: `swap!` (note: NOT hygienic; users must avoid name collisions)
; This works because temp-storage uses a name unlikely to collide
(define-syntax swap-via-vec!
  (syntax-rules ()
    ((_ a b)
     (let ((temp-vec (vector a)))
       (set! a b)
       (set! b (vector-ref temp-vec 0))))))
(define sa 10)
(define sb 20)
(swap-via-vec! sa sb)
(test-eqv "swap-result-a" 20 sa)
(test-eqv "swap-result-b" 10 sb)

; Build do-loop macro (limited form)
(define-syntax dotimes
  (syntax-rules ()
    ((_ (i n) body ...)
     (do ((i 0 (+ i 1)))
         ((= i n))
       body ...))))
(define dt-sum 0)
(dotimes (i 5) (set! dt-sum (+ dt-sum i)))
(test-eqv "dotimes-sum-0-4" 10 dt-sum)

; Multi-clause with literal keyword
(define-syntax cond-eq
  (syntax-rules (=> else)
    ((_ (else expr ...)) (begin expr ...))
    ((_ (test expr ...)) (if test (begin expr ...) #f))
    ((_ (test expr ...) clause ...)
     (if test (begin expr ...) (cond-eq clause ...)))))
(test-eqv "cond-eq-first"  'a (cond-eq (#t 'a) (#t 'b)))
(test-eqv "cond-eq-second" 'b (cond-eq (#f 'a) (#t 'b)))
(test-eqv "cond-eq-else"   'c (cond-eq (#f 'a) (#f 'b) (else 'c)))

; Mixed pattern: fixed prefix + ellipsis
(define-syntax with-tag
  (syntax-rules ()
    ((_ tag x ...) (list (quote tag) x ...))))
(test-equal "fixed-prefix"
  '(my-tag 1 2 3)
  (with-tag my-tag 1 2 3))
(test-equal "fixed-prefix-empty-ellipsis"
  '(only)
  (with-tag only))
