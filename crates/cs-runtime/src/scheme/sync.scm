;;; (crab sync) — atoms: atomic mutable reference cells.
;;;
;;; A bundled Scheme library (global at startup; `(import (crab sync))`
;;; is a no-op). The answer to Clojure's atoms / Go's sync/atomic /
;;; Python's threading primitives. The CrabScheme runtime is currently
;;; single-threaded, so every update is trivially atomic; the API is in
;;; place for when shared-state concurrency lands.
;;;
;;; An atom is `#(__atom__ value)`.

;; Create an atom holding `v`.
(define (make-atom v) (vector '__atom__ v))

;; Atom predicate.
(define (atom? x)
  (and (vector? x) (= (vector-length x) 2) (eq? (vector-ref x 0) '__atom__)))

;; Current value.
(define (atom-deref a) (vector-ref a 1))

;; Set to `v`, returning `v`.
(define (atom-set! a v) (vector-set! a 1 v) v)

;; Set to `(apply f current args)`, returning the new value.
(define (atom-swap! a f . args)
  (let ((nv (apply f (vector-ref a 1) args)))
    (vector-set! a 1 nv)
    nv))

;; Compare-and-set: if the current value is `eqv?` to `old`, store `new`
;; and return #t; otherwise return #f.
(define (atom-cas! a old new)
  (if (eqv? (vector-ref a 1) old)
      (begin (vector-set! a 1 new) #t)
      #f))
