;;; (crab functional) — function combinators.
;;;
;;; A bundled Scheme library: its definitions are evaluated into the
;;; global environment at runtime startup, so `(import (crab
;;; functional))` is a no-op (the same convention the Rust-backed
;;; `(crab …)` modules follow — names are always present). The answer
;;; to Python's functools and Clojure's core combinators.
;;;
;;; All names here are fresh (they don't shadow built-ins).

;; The identity function.
(define (identity x) x)

;; A procedure that ignores its arguments and always returns `x`.
(define (constantly x)
  (lambda args x))

;; Logical negation of a predicate.
(define (complement f)
  (lambda args (not (apply f args))))

;; Swap the argument order of a binary procedure.
(define (flip f)
  (lambda (a b) (f b a)))

;; Right-to-left composition: ((compose f g) x) = (f (g x)).
;; With no arguments, returns `identity`.
(define (compose . fs)
  (cond
    ((null? fs) identity)
    ((null? (cdr fs)) (car fs))
    (else
     (let ((rfs (reverse fs)))
       (lambda args
         (let loop ((gs (cdr rfs))
                    (v (apply (car rfs) args)))
           (if (null? gs)
               v
               (loop (cdr gs) ((car gs) v)))))))))

;; Left-to-right composition: ((pipe f g) x) = (g (f x)).
(define (pipe . fs)
  (apply compose (reverse fs)))

;; Partial application: prepend `bound` to the arguments of `f`.
(define (partial f . bound)
  (lambda args (apply f (append bound args))))

;; Apply each of `fs` to the same arguments, returning the list of
;; results: ((juxt f g) x) = (list (f x) (g x)).
(define (juxt . fs)
  (lambda args
    (map (lambda (f) (apply f args)) fs)))

;; Like `f`, but a #f first argument is replaced by `default`.
(define (fnil f default)
  (lambda (x . rest)
    (apply f (if x x default) rest)))

;; Memoize `f` on its argument list (compared with `equal?`).
;; Caches #f results correctly by boxing values in a one-element list.
(define (memoize f)
  (let ((cache (make-hashtable equal-hash equal?)))
    (lambda args
      (let ((cell (hashtable-ref cache args #f)))
        (if cell
            (car cell)
            (let ((v (apply f args)))
              (hashtable-set! cache args (list v))
              v))))))
