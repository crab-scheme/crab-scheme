;;; (crab walk) — recursive tree walking and transformation.
;;;
;;; A bundled Scheme library (global at startup; `(import (crab walk))`
;;; is a no-op). The answer to Clojure's clojure.walk. Operates on proper
;;; lists and vectors; other values pass through unchanged.

;; Apply `inner` to each element of `form`, rebuild the same kind of
;; collection, then apply `outer` to that. Atoms → (outer form).
(define (walk inner outer form)
  (cond
    ((and (pair? form) (list? form)) (outer (map inner form)))
    ((vector? form) (outer (vector-map inner form)))
    (else (outer form))))

;; Apply `f` bottom-up: transform children first, then the node itself.
(define (postwalk f form)
  (walk (lambda (x) (postwalk f x)) f form))

;; Apply `f` top-down: transform the node first, then recurse.
(define (prewalk f form)
  (walk (lambda (x) (prewalk f x)) (lambda (x) x) (f form)))

;; Replace any node `equal?` to a key of the alist `smap` with its value,
;; bottom-up / top-down respectively.
(define (postwalk-replace smap form)
  (postwalk (lambda (x) (let ((p (assoc x smap))) (if p (cdr p) x))) form))

(define (prewalk-replace smap form)
  (prewalk (lambda (x) (let ((p (assoc x smap))) (if p (cdr p) x))) form))
