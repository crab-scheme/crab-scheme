; Persistent (immutable) ordered map — in CrabScheme.
;
; A pure value (Constitution Article II): every `pmap-set`/`pmap-del` returns a
; NEW map and never mutates the old one — so it can back a consensus state
; machine where each node keeps its own immutable snapshot. CrabScheme ships
; only *mutable* R6RS hashtables (no persistent map), so this fills the gap and
; gives an O(log n) pure map instead of the O(n) association list.
;
; Implemented as a treap: a binary search tree (ordered by the map's `key<?`)
; that is also a heap on a per-key priority. Priorities come from `equal-hash`,
; so the tree is balanced in expectation without needing a random source —
; deterministic, which the engines require.
;
; API:
;   (pmap key<?)            -> empty map ordered by key<?
;   (pmap-ref m k default)  -> value or default
;   (pmap-set m k v)        -> map' with k=v
;   (pmap-del m k)          -> map' without k
;   (pmap-size m)           -> count
;   (pmap-fold m f acc)     -> fold (f key val acc) in ascending key order

; node = (key val priority left right); empty subtree = '()
(define (pm:nd k v p l r) (list k v p l r))
(define (pm:k n) (list-ref n 0))
(define (pm:v n) (list-ref n 1))
(define (pm:p n) (list-ref n 2))
(define (pm:l n) (list-ref n 3))
(define (pm:r n) (list-ref n 4))

(define (pm:rot-right n)                ; left child becomes root
  (let ((l (pm:l n)))
    (pm:nd (pm:k l) (pm:v l) (pm:p l) (pm:l l)
           (pm:nd (pm:k n) (pm:v n) (pm:p n) (pm:r l) (pm:r n)))))
(define (pm:rot-left n)                 ; right child becomes root
  (let ((r (pm:r n)))
    (pm:nd (pm:k r) (pm:v r) (pm:p r)
           (pm:nd (pm:k n) (pm:v n) (pm:p n) (pm:l n) (pm:l r)) (pm:r r))))

(define (pm:ins t k< k v)
  (if (null? t)
      (pm:nd k v (equal-hash k) '() '())
      (cond
        ((k< k (pm:k t))
         (let* ((nl (pm:ins (pm:l t) k< k v))
                (n (pm:nd (pm:k t) (pm:v t) (pm:p t) nl (pm:r t))))
           (if (< (pm:p nl) (pm:p n)) (pm:rot-right n) n)))   ; min-heap on priority
        ((k< (pm:k t) k)
         (let* ((nr (pm:ins (pm:r t) k< k v))
                (n (pm:nd (pm:k t) (pm:v t) (pm:p t) (pm:l t) nr)))
           (if (< (pm:p nr) (pm:p n)) (pm:rot-left n) n)))
        (else (pm:nd k v (pm:p t) (pm:l t) (pm:r t))))))      ; update value

(define (pm:get t k< k)
  (cond ((null? t) #f)
        ((k< k (pm:k t)) (pm:get (pm:l t) k< k))
        ((k< (pm:k t) k) (pm:get (pm:r t) k< k))
        (else t)))

(define (pm:del t k< k)
  (cond
    ((null? t) '())
    ((k< k (pm:k t)) (pm:nd (pm:k t) (pm:v t) (pm:p t) (pm:del (pm:l t) k< k) (pm:r t)))
    ((k< (pm:k t) k) (pm:nd (pm:k t) (pm:v t) (pm:p t) (pm:l t) (pm:del (pm:r t) k< k)))
    (else
     (cond
       ((null? (pm:l t)) (pm:r t))
       ((null? (pm:r t)) (pm:l t))
       ((< (pm:p (pm:l t)) (pm:p (pm:r t)))
        (let ((r (pm:rot-right t)))
          (pm:nd (pm:k r) (pm:v r) (pm:p r) (pm:l r) (pm:del (pm:r r) k< k))))
       (else
        (let ((r (pm:rot-left t)))
          (pm:nd (pm:k r) (pm:v r) (pm:p r) (pm:del (pm:l r) k< k) (pm:r r))))))))

(define (pm:fold t f acc)               ; ascending key order
  (if (null? t) acc
      (pm:fold (pm:r t) f (f (pm:k t) (pm:v t) (pm:fold (pm:l t) f acc)))))

; ---- public surface: a map is (key<? . root) ----
(define (pmap key<?) (cons key<? '()))
(define (pmap-ref m k default)
  (let ((n (pm:get (cdr m) (car m) k))) (if n (pm:v n) default)))
(define (pmap-set m k v) (cons (car m) (pm:ins (cdr m) (car m) k v)))
(define (pmap-del m k) (cons (car m) (pm:del (cdr m) (car m) k)))
(define (pmap-fold m f acc) (pm:fold (cdr m) f acc))
(define (pmap-size m) (pmap-fold m (lambda (k v a) (+ a 1)) 0))

; The self-test lives in pmap-test.scm so this file can be `include`d as a
; pure library with no side effects (Article V) — `crabscheme run
; lib/consensus/pmap-test.scm` exercises it.
