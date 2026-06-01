;;; (crab dict) — association-list helpers, including nested access.
;;;
;;; A bundled Scheme library, evaluated into the global environment at
;;; startup (so `(import (crab dict))` is a no-op). The answer to
;;; Clojure's map functions (get-in / assoc-in / update-in / merge /
;;; select-keys) and Python's dict conveniences.
;;;
;;; A "dict" here is an association list `((key . value) …)`; keys are
;;; compared with `equal?`. All operations are functional — they return
;;; a new alist rather than mutating. (Unlike a hashtable, these are
;;; immutable and order-preserving; for large dictionaries the lookups
;;; are linear.)

;; (dict-ref d key [default]) — value for `key`, or `default` (or #f).
(define (dict-ref d key . default)
  (let ((p (assoc key d)))
    (cond (p (cdr p))
          ((null? default) #f)
          (else (car default)))))

(define (dict-has? d key)
  (if (assoc key d) #t #f))

;; Return a new dict with `key` set to `value` (replacing any existing
;; binding in place, otherwise appended — order is preserved).
(define (dict-set d key value)
  (if (assoc key d)
      (map (lambda (p) (if (equal? (car p) key) (cons key value) p)) d)
      (append d (list (cons key value)))))

;; Return a new dict without `key`.
(define (dissoc d key)
  (filter (lambda (p) (not (equal? (car p) key))) d))

(define (dict-keys d) (map car d))
(define (dict-vals d) (map cdr d))

;; Build a dict from parallel key and value lists.
(define (zipmap keys vals) (map cons keys vals))

;; Keep only the entries whose keys appear in `keys`.
(define (select-keys d keys)
  (filter (lambda (p) (member (car p) keys)) d))

;; (get-in d path [default]) — follow `path` (a list of keys) through
;; nested dicts; return the value found, or `default` (or #f).
(define (get-in d path . default)
  (let loop ((d d) (path path))
    (if (null? path)
        d
        (let ((p (and (pair? d) (assoc (car path) d))))
          (if p
              (loop (cdr p) (cdr path))
              (if (null? default) #f (car default)))))))

;; Set the value at nested `path`, creating intermediate dicts as
;; needed; returns a new nested structure.
(define (assoc-in d path value)
  (if (null? (cdr path))
      (dict-set d (car path) value)
      (dict-set d (car path)
                (assoc-in (dict-ref d (car path) '()) (cdr path) value))))

;; Apply `f` to the value at nested `path` and store the result.
(define (update-in d path f)
  (assoc-in d path (f (get-in d path))))

;; Merge dicts left-to-right; later dicts win on key collisions.
(define (merge . dicts)
  (fold-left
   (lambda (acc d)
     (fold-left (lambda (acc p) (dict-set acc (car p) (cdr p))) acc d))
   '()
   dicts))
