;;; (crab pprint) — pretty-printing for nested data.
;;;
;;; A bundled Scheme library, evaluated into the global environment at
;;; startup (so `(import (crab pprint))` is a no-op). The answer to
;;; Python's pprint and clojure.pprint.
;;;
;;; Strategy: a value is printed on one line via `write` when its
;;; compact form fits in `*pp-width*` columns; otherwise proper lists
;;; and vectors are broken one element per line, aligned under the
;;; opening bracket, recursively. Atoms, dotted pairs, and other
;;; objects always use the compact `write` form.

;; Target line width (columns). Rebind with `set!` to taste.
(define *pp-width* 64)

;; The one-line `write` representation of `x`.
(define (pp-compact x)
  (let ((port (open-output-string)))
    (write x port)
    (get-output-string port)))

(define (pp-spaces n) (make-string n #\space))

;; Render the elements of `parts` (already-formatted strings) under an
;; opening bracket `open` at column `inner`, closing with `close`.
(define (pp-join open parts inner close)
  (if (null? parts)
      (string-append open close)
      (string-append
       open
       (car parts)
       (apply string-append
              (map (lambda (p) (string-append "\n" (pp-spaces inner) p))
                   (cdr parts)))
       close)))

(define (pp-format x indent)
  (let ((c (pp-compact x)))
    (if (<= (+ indent (string-length c)) *pp-width*)
        c
        (cond
          ((and (pair? x) (list? x))
           (let ((inner (+ indent 1)))
             (pp-join "(" (map (lambda (e) (pp-format e inner)) x) inner ")")))
          ((vector? x)
           (let ((inner (+ indent 2)))
             (pp-join "#("
                      (map (lambda (e) (pp-format e inner)) (vector->list x))
                      inner ")")))
          (else c)))))

;; Return the pretty-printed string for `x` (no trailing newline).
(define (pretty-format x) (pp-format x 0))

;; Pretty-print `x` followed by a newline.
(define (pretty-print x)
  (display (pretty-format x))
  (newline))

;; Short alias.
(define pp pretty-print)
