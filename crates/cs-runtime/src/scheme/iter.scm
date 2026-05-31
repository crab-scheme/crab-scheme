;;; (crab iter) — itertools-style sequence operations + lazy streams.
;;;
;;; A bundled Scheme library, evaluated into the global environment at
;;; startup (so `(import (crab iter))` is a no-op). The answer to
;;; Python's itertools and Clojure's lazy seqs.
;;;
;;; CrabScheme already provides `take`, `drop`, `take-while`,
;;; `drop-while`, `partition`, and `zip` as built-ins, so this module
;;; supplies the rest: grouping/counting, ranges, de-duplication, and
;;; a small lazy-stream facility for infinite sequences.

;; --- eager list operations ---

;; (range end) | (range start end) | (range start end step).
;; Half-open: the result excludes `end`. `step` may be negative.
;; (Explicit `lambda` rest list — the `(define (range . args) …)` sugar
;; isn't accepted by the reader.)
(define range
  (lambda args
    (let ((start (if (null? (cdr args)) 0 (car args)))
          (end (if (null? (cdr args)) (car args) (cadr args)))
          (step (if (or (null? (cdr args)) (null? (cddr args))) 1 (caddr args))))
      (when (= step 0) (error "range: step must be non-zero"))
      (let loop ((i start) (acc '()))
        (if (if (> step 0) (>= i end) (<= i end))
            (reverse acc)
            (loop (+ i step) (cons i acc)))))))

;; Remove duplicates (compared with `equal?`), keeping first occurrence
;; and preserving order.
;;
;; NB: the grouping/dedup procedures here use only hashtable-ref and
;; hashtable-set! (with a unique sentinel for "absent"). hashtable-
;; contains? / hashtable-update! currently hit an internal-error on
;; custom-equiv (equal?) hashtables in the runtime, so we route around
;; them. (Tracked as a runtime bug to fix separately.)
(define (distinct lst)
  (let ((seen (make-hashtable equal-hash equal?))
        (missing (list 'missing)))
    (let loop ((lst lst) (acc '()))
      (cond
        ((null? lst) (reverse acc))
        ((eq? (hashtable-ref seen (car lst) missing) missing)
         (hashtable-set! seen (car lst) #t)
         (loop (cdr lst) (cons (car lst) acc)))
        (else (loop (cdr lst) acc))))))

;; (iterate f x n) = (x (f x) (f (f x)) …) of length n.
(define (iterate f x n)
  (if (<= n 0)
      '()
      (cons x (iterate f (f x) (- n 1)))))

;; Group `lst` by (key-fn element); returns an alist of
;; (key . (elements…)) with keys in first-seen order and each group in
;; original order.
(define (group-by key-fn lst)
  (let ((tbl (make-hashtable equal-hash equal?))
        (missing (list 'missing))
        (order '()))
    (for-each
     (lambda (x)
       (let* ((k (key-fn x))
              (cur (hashtable-ref tbl k missing)))
         (if (eq? cur missing)
             (begin (set! order (cons k order))
                    (hashtable-set! tbl k (list x)))
             (hashtable-set! tbl k (cons x cur)))))
     lst)
    (map (lambda (k) (cons k (reverse (hashtable-ref tbl k '()))))
         (reverse order))))

;; Count occurrences; returns an alist of (element . count), elements
;; in first-seen order.
(define (frequencies lst)
  (let ((tbl (make-hashtable equal-hash equal?))
        (missing (list 'missing))
        (order '()))
    (for-each
     (lambda (x)
       (let ((cur (hashtable-ref tbl x missing)))
         (if (eq? cur missing)
             (begin (set! order (cons x order))
                    (hashtable-set! tbl x 1))
             (hashtable-set! tbl x (+ cur 1)))))
     lst)
    (map (lambda (x) (cons x (hashtable-ref tbl x 0))) (reverse order))))

;; Split `lst` into consecutive sublists of length `n` (the last may be
;; shorter). Uses nested named-lets (no built-in take/drop dependency,
;; no internal define).
(define (chunk lst n)
  (when (<= n 0) (error "chunk: n must be positive"))
  (let loop ((lst lst) (acc '()))
    (if (null? lst)
        (reverse acc)
        (let take-n ((rest lst) (k n) (head '()))
          (if (or (= k 0) (null? rest))
              (loop rest (cons (reverse head) acc))
              (take-n (cdr rest) (- k 1) (cons (car rest) head)))))))

;; Interleave elements of several lists, stopping at the shortest.
(define interleave
  (lambda lsts
    (if (or (null? lsts) (memv '() lsts))
        '()
        (append (map car lsts) (apply interleave (map cdr lsts))))))

;; Deeply flatten a nested list into a flat list.
(define (flatten lst)
  (cond
    ((null? lst) '())
    ((pair? (car lst)) (append (flatten (car lst)) (flatten (cdr lst))))
    (else (cons (car lst) (flatten (cdr lst))))))

;; Count elements satisfying `pred`.
(define (count-if pred lst)
  (fold-left (lambda (acc x) (if (pred x) (+ acc 1) acc)) 0 lst))

;; --- lazy streams (for infinite / unbounded sequences) ---
;;
;; A stream is `'()` or `(value . promise-of-rest)`. `stream-cons`
;; delays the tail so streams can be infinite.

(define-syntax stream-cons
  (syntax-rules ()
    ((_ a b) (cons a (delay b)))))

(define (stream-car s) (car s))
(define (stream-cdr s) (force (cdr s)))
(define (stream-null? s) (null? s))
(define stream-nil '())

;; Take the first `n` values of a stream as an ordinary list.
(define (stream-take s n)
  (if (or (<= n 0) (stream-null? s))
      '()
      (cons (stream-car s) (stream-take (stream-cdr s) (- n 1)))))

;; Lazily map `f` over a stream.
(define (stream-map f s)
  (if (stream-null? s)
      stream-nil
      (stream-cons (f (stream-car s)) (stream-map f (stream-cdr s)))))

;; Lazily filter a stream by `pred`.
(define (stream-filter pred s)
  (cond
    ((stream-null? s) stream-nil)
    ((pred (stream-car s))
     (stream-cons (stream-car s) (stream-filter pred (stream-cdr s))))
    (else (stream-filter pred (stream-cdr s)))))

;; Infinite stream: (x (f x) (f (f x)) …).
(define (stream-iterate f x)
  (stream-cons x (stream-iterate f (f x))))

;; Infinite stream of `x` repeated.
(define (stream-repeat x)
  (stream-cons x (stream-repeat x)))

;; The infinite stream 0, 1, 2, ….
(define naturals (stream-iterate (lambda (n) (+ n 1)) 0))
