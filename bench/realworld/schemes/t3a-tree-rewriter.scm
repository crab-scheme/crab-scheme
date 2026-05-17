; T3-A — tree-rewriter (heap pressure + sustained alloc).
;
; Builds a symbolic expression tree of fixed shape, then repeatedly
; applies a normalization pass (constant folding + a couple algebraic
; identities) that ALLOCATES a fresh tree each pass and drops the
; previous one. Steady-state live heap oscillates between the input
; tree and the under-construction output.
;
; The expression grammar is a small subset of Scheme arithmetic:
;   expr := <fixnum>
;        | (+ expr expr)
;        | (- expr expr)
;        | (* expr expr)
;        | (if expr expr expr)
;
; Normalization rules (applied bottom-up via a recursive walk):
;   (+ 0 x) → x          (additive identity, left)
;   (+ x 0) → x          (additive identity, right)
;   (* 0 x) → 0          (multiplicative absorbing, left)
;   (* x 0) → 0          (...right)
;   (* 1 x) → x          (multiplicative identity, left)
;   (* x 1) → x          (...right)
;   (- x x) → 0          (self-cancellation, only when x is a fixnum)
;   (if 0 a b) → b       (constant-condition fold; Scheme treats 0
;                         as truthy, so we treat 0 as the "false"
;                         marker for this synthetic bench)
;   (<op> <fixnum> <fixnum>) → <fold>  (arithmetic constant fold)
;
; Each iter: rebuild input tree (so the rewriter has fresh garbage
; to chew on), run one normalization pass, return the resulting
; tree. The rebuild itself dominates allocation; the rewriter is
; mostly structure-sharing for already-normalized branches.

(define tree-depth 14)
; depth 14 → ~16k leaves, ~32k nodes per tree. With rebuild +
; rewrite per iter, each iter allocates roughly 100-200k pairs.

(define (build-tree depth seed)
  (if (= depth 0)
      seed
      (let* ((d1 (- depth 1))
             (s2 (modulo (* seed 1103515245) 2147483648))
             (op-choice (modulo seed 5))
             (left (build-tree d1 (modulo (+ s2 1) 2147483648)))
             (right (build-tree d1 (modulo (+ s2 7919) 2147483648))))
        (cond ((= op-choice 0) (list '+ left right))
              ((= op-choice 1) (list '- left right))
              ((= op-choice 2) (list '* left right))
              ((= op-choice 3) (list 'if left right right))
              (else (modulo s2 100))))))

(define (fixnum-expr? x) (number? x))

(define (rewrite expr)
  (cond
    ((fixnum-expr? expr) expr)
    ((pair? expr)
     (let ((op (car expr)))
       (cond
         ((eq? op '+)
          (let ((a (rewrite (car (cdr expr))))
                (b (rewrite (car (cdr (cdr expr))))))
            (cond
              ((and (fixnum-expr? a) (fixnum-expr? b)) (+ a b))
              ((and (fixnum-expr? a) (= a 0)) b)
              ((and (fixnum-expr? b) (= b 0)) a)
              (else (list '+ a b)))))
         ((eq? op '-)
          (let ((a (rewrite (car (cdr expr))))
                (b (rewrite (car (cdr (cdr expr))))))
            (cond
              ((and (fixnum-expr? a) (fixnum-expr? b)) (- a b))
              ((and (fixnum-expr? a) (fixnum-expr? b) (= a b)) 0)
              (else (list '- a b)))))
         ((eq? op '*)
          (let ((a (rewrite (car (cdr expr))))
                (b (rewrite (car (cdr (cdr expr))))))
            (cond
              ((and (fixnum-expr? a) (fixnum-expr? b)) (* a b))
              ((and (fixnum-expr? a) (= a 0)) 0)
              ((and (fixnum-expr? b) (= b 0)) 0)
              ((and (fixnum-expr? a) (= a 1)) b)
              ((and (fixnum-expr? b) (= b 1)) a)
              (else (list '* a b)))))
         ((eq? op 'if)
          (let ((c (rewrite (car (cdr expr))))
                (t (rewrite (car (cdr (cdr expr)))))
                (e (rewrite (car (cdr (cdr (cdr expr)))))))
            (cond
              ((and (fixnum-expr? c) (= c 0)) e)
              ((fixnum-expr? c) t)
              (else (list 'if c t e)))))
         (else expr))))
    (else expr)))

; Verification: count nodes so we can sanity-check that the rewriter
; produces SOMETHING (not just an empty result). The exact count
; depends on how many fold-ables the random seed produced.
(define (count-nodes expr)
  (if (pair? expr)
      (let loop ((rest (cdr expr)) (acc 1))
        (if (null? rest)
            acc
            (loop (cdr rest) (+ acc (count-nodes (car rest))))))
      1))

(define (one-pass seed)
  (let* ((tree (build-tree tree-depth seed))
         (rewritten (rewrite tree)))
    (count-nodes rewritten)))

(realworld-bench
  "t3a-tree-rewriter"
  (list (cons (quote tree-depth) tree-depth))
  (lambda () (one-pass 42)))
