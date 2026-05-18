# `(crab collection)` — FIFO queue, hash set, priority heap

CrabScheme stdlib module. Iter 12 of the stdlib-modules spec.
R6RS covers ordered associations (lists, alists, hashtables) and
vectors; this module fills in the data structures every program
eventually needs on top.

Handles are fixnums into a thread-local slab — CrabScheme is
single-threaded, so per-runtime-thread storage is correct and
avoids the Mutex/Sync ceremony. Typed `queue?` / `set?` / `heap?`
predicates land with `Value::Opaque`.

## Procedures

```
;; Queue — FIFO of Values
(queue-new)              ;-> handle
(queue-push! q val)      ;-> unspec
(queue-pop! q)           ;-> val or #f
(queue-peek q)           ;-> val or #f
(queue-length q)         ;-> fixnum
(queue-empty? q)         ;-> boolean

;; Set — string-keyed unordered uniqueness
(set-new)                ;-> handle
(set-add! s string)      ;-> unspec
(set-remove! s string)   ;-> boolean  ; true if it was present
(set-contains? s string) ;-> boolean
(set-size s)             ;-> fixnum
(set->list s)            ;-> list of strings  ; order unspecified

;; Heap — max-heap of fixnums
(heap-new)               ;-> handle
(heap-push! h fixnum)    ;-> unspec
(heap-pop! h)            ;-> fixnum or #f
(heap-peek h)            ;-> fixnum or #f
(heap-length h)          ;-> fixnum
```

## Iter-12 scope caveats

- **Sets are string-keyed only.** General-Value sets need a stable
  hash over Scheme values (`equal-hash`); tracked for a follow-up.
- **Heaps are max-heaps of fixnums.** General-Value priority
  queues need a comparator argument; for min-heap behavior today,
  push `(- x)` and negate on pop. R6RS already has `list-sort` /
  `vector-sort` for whole-collection sorts.

## Example

```scheme
(import (crab collection))

;; Job queue
(define jobs (queue-new))
(queue-push! jobs "alpha")
(queue-push! jobs "beta")
(let loop ()
  (let ((job (queue-pop! jobs)))
    (when job
      (display "processing ") (display job) (newline)
      (loop))))

;; Deduplicate a list of strings
(define seen (set-new))
(for-each (lambda (s) (set-add! seen s)) some-list)
(display (set-size seen)) (display " unique") (newline)

;; Top-3 from a stream
(define h (heap-new))
(for-each (lambda (n) (heap-push! h n)) '(3 1 4 1 5 9 2 6 5 3 5))
(let loop ((n 3) (acc '()))
  (if (zero? n)
      (display (reverse acc))
      (loop (- n 1) (cons (heap-pop! h) acc))))
;; (9 6 5)
```
