(test-section "R7RS case with => arrow form")

; --- (case key ((d ...) => proc) ...) calls proc with the matched key ---
(test-eqv "case-arrow-match" 6
  (case 3
    ((1 2 3) => (lambda (x) (* x 2)))
    (else 0)))

; --- proc is called with the original key (not the matched datum list) ---
(test-equal "case-arrow-key-passed-thru" 'pi
  (case 'pi
    ((e tau pi phi) => (lambda (sym) sym))
    (else 'none)))

; --- arrow with no match falls through to next clause ---
(test-eqv "case-arrow-fallthrough" 99
  (case 'foo
    ((a b c) => (lambda (s) -1))
    (else 99)))

; --- arrow on else clause ---
(test-equal "case-arrow-else" "got-unknown"
  (case 'unknown
    ((known) "known-result")
    (else => (lambda (k) (string-append "got-" (symbol->string k))))))

; --- arrow proc returns multiple values via call-with-values ---
(test-eqv "case-arrow-mv-via-cwv" 30
  (call-with-values
    (lambda ()
      (case 5
        ((5) => (lambda (k) (values k (* k 5))))
        (else => (lambda (k) (values k 0)))))
    +))

; --- non-arrow body in same case mixes with arrow body ---
(test-eqv "case-mix-arrow-and-body-1" 100
  (case 1
    ((1) => (lambda (x) (* x 100)))
    ((2 3) 'two-or-three)
    (else 'none)))
(test-equal "case-mix-arrow-and-body-2" 'two-or-three
  (case 2
    ((1) => (lambda (x) (* x 100)))
    ((2 3) 'two-or-three)
    (else 'none)))

; --- (case key ((d) => proc) (else => proc)) — both arrows ---
(define (handler v) (string-append "v=" (number->string v)))
(test-equal "case-both-arrow-match" "v=42"
  (case 42 ((42) => handler) (else => handler)))
(test-equal "case-both-arrow-else" "v=999"
  (case 999 ((1 2 3) => handler) (else => handler)))

; --- regression: non-arrow case still works ---
(test-equal "case-classic" 'small
  (case 3
    ((1 2 3) 'small)
    ((4 5 6) 'medium)
    (else 'big)))
