; (crab json) realworld bench — stdlib-modules iter 15.
;
; Builds a list of N record-shaped alists, stringifies the whole
; payload, parses it back, and asserts the round-trip preserves
; record count. Exercises both the encode and decode paths of
; cs-stdlib-json. Result returned for chez-shim is the record
; count (an integer) — though Chez doesn't have (crab json), so
; this bench only runs under crabscheme.

(define (build-records n)
  ; n alists, each shaped:
  ;   (("age" . i) ("items" . (i*1 i*2 i*3)) ("name" . "alice"))
  ; (keys alphabetical so the alist matches what json-parse will
  ; hand back after the round-trip — cs-stdlib-json sorts object
  ; keys alphabetically.)
  (let loop ((i 0) (acc '()))
    (if (= i n)
        (reverse acc)
        (loop (+ i 1)
              (cons
                (list (cons "age" i)
                      (cons "items" (list (* i 1) (* i 2) (* i 3)))
                      (cons "name" "alice"))
                acc)))))

(define __records__ (build-records 200))

(realworld-bench
  "crab-json"
  '((records . 200))
  (lambda ()
    (let* ((encoded (json-stringify __records__))
           (decoded (json-parse encoded)))
      (length decoded))))
