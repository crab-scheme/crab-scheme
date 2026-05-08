; Conformance test prelude.
;
; Each test file loads this implicitly via the runner. The runner reads the
; final `__test-results__` binding to extract pass/fail counts.
;
; Usage:
;   (test-eqv "name" expected actual)
;   (test-equal "name" expected actual)
;   (test-true "name" actual)
;   (test-false "name" actual)
;   (test-section "section name")  ; cosmetic; collected by harness

(define __pass-count__ 0)
(define __fail-count__ 0)
(define __failures__ '())
(define __cur-section__ "")

(define (test-section name)
  (set! __cur-section__ name))

(define (test-eqv name expected actual)
  (if (eqv? expected actual)
      (set! __pass-count__ (+ __pass-count__ 1))
      (begin
        (set! __fail-count__ (+ __fail-count__ 1))
        (set! __failures__
              (cons (list __cur-section__ name expected actual) __failures__)))))

(define (test-equal name expected actual)
  (if (equal? expected actual)
      (set! __pass-count__ (+ __pass-count__ 1))
      (begin
        (set! __fail-count__ (+ __fail-count__ 1))
        (set! __failures__
              (cons (list __cur-section__ name expected actual) __failures__)))))

(define (test-true name actual)
  (if actual
      (set! __pass-count__ (+ __pass-count__ 1))
      (begin
        (set! __fail-count__ (+ __fail-count__ 1))
        (set! __failures__
              (cons (list __cur-section__ name 'true actual) __failures__)))))

(define (test-false name actual)
  (if (not actual)
      (set! __pass-count__ (+ __pass-count__ 1))
      (begin
        (set! __fail-count__ (+ __fail-count__ 1))
        (set! __failures__
              (cons (list __cur-section__ name 'false actual) __failures__)))))

(define (__test-summary__)
  (list __pass-count__ __fail-count__ (reverse __failures__)))
