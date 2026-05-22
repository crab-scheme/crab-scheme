; Self-test for the persistent map (lib/consensus/pmap.scm).
;
;   crabscheme run lib/consensus/pmap-test.scm
;
; Kept separate from pmap.scm so that file is a side-effect-free library the
; engines can `include` without running (or printing) a test.

(include "lib/consensus/pmap.scm")

(define pm-fail 0)
(define (pm-check name expected actual)
  (if (equal? expected actual)
      (begin (display "  ok   ") (display name) (newline))
      (begin (set! pm-fail (+ pm-fail 1))
             (display "  FAIL ") (display name)
             (display " expected=") (write expected) (display " got=") (write actual) (newline))))

; insert 0..99 (in order — would degenerate an unbalanced BST), then query.
(define m100
  (let loop ((i 0) (m (pmap string<?)))
    (if (>= i 100) m (loop (+ i 1) (pmap-set m (string-append "k" (number->string i)) i)))))
(pm-check "size-100" 100 (pmap-size m100))
(pm-check "ref-k0"   0   (pmap-ref m100 "k0" #f))
(pm-check "ref-k57"  57  (pmap-ref m100 "k57" #f))
(pm-check "ref-miss" #f  (pmap-ref m100 "nope" #f))
; immutability: deleting from a copy doesn't touch the original
(define m99 (pmap-del m100 "k57"))
(pm-check "del-gone"      #f (pmap-ref m99 "k57" #f))
(pm-check "orig-untouched" 57 (pmap-ref m100 "k57" #f))
(pm-check "size-after-del" 99 (pmap-size m99))
; update keeps size, changes value
(define m100b (pmap-set m100 "k57" 999))
(pm-check "update-val"  999 (pmap-ref m100b "k57" #f))
(pm-check "update-size" 100 (pmap-size m100b))

(newline)
(if (> pm-fail 0) (error "pmap self-test FAILED" pm-fail)
    (begin (display "pmap self-test: all checks passed") (newline)))
