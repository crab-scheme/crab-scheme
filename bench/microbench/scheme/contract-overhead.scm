; contract-overhead.scm — micro-benchmark for Phase 2B.7 eta-elision.
;
; Compares per-call overhead of:
;   A) uncontracted procedure call (baseline)
;   B) contracted call via the 2B.7 fast path (monomorphic simple predicates)
;   C) old-style wrapper that always calls __apply-domain / __apply-range
;      (replicates pre-2B.7 per-call cost for all-predicate contracts)
;
; Self-contained — does not load lib/contract/contract.scm.

; ---- minimal contract machinery ----

(define (make-contract domain-preds range-pred)
  (vector '__contract__ domain-preds range-pred))
(define (contract? c)
  (and (vector? c) (>= (vector-length c) 1) (eq? (vector-ref c 0) '__contract__)))
(define (contract-domains c) (vector-ref c 1))
(define (contract-range c)   (vector-ref c 2))
(define (contract-rest c)    (if (>= (vector-length c) 4) (vector-ref c 3) #f))
(define (make-contract-violation blame name desc val)
  (vector '__cv__ blame name desc val))

(define (__reject-region-or v blame name desc)
  (if (eq? (gc-allocator v) 'region)
      (raise (make-contract-violation blame name desc v))
      v))

; ---- 2B.7 fast path helpers ----

(define (__all-simple-preds? doms rng)
  (and (procedure? rng) (not (contract? rng))
       (let loop ((ds doms))
         (cond ((null? ds) #t)
               ((contract? (car ds)) #f)
               ((procedure? (car ds)) (loop (cdr ds)))
               (else #f)))))

(define (__fast-fixed proc name desc doms rng n)
  (lambda args
    (if (not (= (length args) n))
        (raise (make-contract-violation 'caller name desc
                 (list 'arity-mismatch 'expected n 'got (length args)))))
    (let* ((checked
            (let loop ((ds doms) (as args) (acc '()))
              (if (null? ds)
                  (reverse acc)
                  (let ((v (car as)))
                    (__reject-region-or v 'caller name desc)
                    (if ((car ds) v)
                        (loop (cdr ds) (cdr as) (cons v acc))
                        (raise (make-contract-violation 'caller name desc v))))))))
      (let ((r (apply proc checked)))
        (__reject-region-or r 'callee name desc)
        (if (rng r) r (raise (make-contract-violation 'callee name desc r)))))))

(define ->
  (lambda preds
    (if (< (length preds) 2) (error '-> "needs domain + range" preds))
    (let loop ((rest preds) (acc '()))
      (if (null? (cdr rest))
          (make-contract (reverse acc) (car rest))
          (loop (cdr rest) (cons (car rest) acc))))))

(define (apply-contract-arrow c proc name)
  (let* ((doms (contract-domains c))
         (rng  (contract-range c))
         (desc (list '-> doms rng)))
    (if (__all-simple-preds? doms rng)
        (__fast-fixed proc name desc doms rng (length doms))
        (error 'bench "slow path not needed in this bench"))))

(define (apply-contract c proc name)
  (if (not (contract? c)) (error 'apply-contract "not a contract" c))
  (if (not (procedure? proc)) (error 'apply-contract "not a procedure" proc))
  (apply-contract-arrow c proc name))

; ---- old-style wrapper (pre-2B.7: __apply-domain / __apply-range per call) ----

(define (__old-apply-domain spec arg name desc)
  (__reject-region-or arg 'caller name desc)
  (cond
    ((contract? spec)
     (raise (make-contract-violation 'caller name desc arg)))
    ((procedure? spec)
     (if (spec arg) arg (raise (make-contract-violation 'caller name desc arg))))
    (else (error 'old "bad spec" spec))))

(define (__old-apply-range spec result name desc)
  (__reject-region-or result 'callee name desc)
  (cond
    ((contract? spec)
     (raise (make-contract-violation 'callee name desc result)))
    ((procedure? spec)
     (if (spec result) result
         (raise (make-contract-violation 'callee name desc result))))
    (else (error 'old "bad spec" spec))))

; Exact pre-2B.7 apply-contract-arrow for fixed-arity.
(define (make-old-style-wrapper proc doms rng)
  (let ((desc (list '-> doms rng)))
    (lambda args
      (if (not (= (length args) (length doms)))
          (raise (make-contract-violation 'caller 'old desc
                   (list 'arity-mismatch 'expected (length doms) 'got (length args)))))
      (let* ((checked-args
              (let loop ((ds doms) (as args) (acc '()))
                (if (null? ds)
                    (reverse acc)
                    (loop (cdr ds)
                          (cdr as)
                          (cons (__old-apply-domain (car ds) (car as) 'old desc)
                                acc)))))
             (result (apply proc checked-args)))
        (__old-apply-range rng result 'old desc)))))

; ---- benchmark ----

(define (raw-add x y) (+ x y))
(define N 500000)

(define (time-thunk thunk)
  (let* ((s (current-jiffy))
         (_ (thunk))
         (e (current-jiffy)))
    (exact->inexact (/ (* (- e s) 1000) (jiffies-per-second)))))

(define fast-add
  (apply-contract (-> number? number? number?) raw-add 'add))

(define old-add
  (make-old-style-wrapper raw-add (list number? number?) number?))

; Warmup
(let warmup ((i 2000))
  (if (> i 0)
      (begin (raw-add 1 2) (fast-add 1 2) (old-add 1 2) (warmup (- i 1)))))

(let* ((t-raw  (time-thunk (lambda ()
                 (let loop ((i N) (acc 0))
                   (if (= i 0) acc (loop (- i 1) (raw-add acc 1)))))))
       (t-fast (time-thunk (lambda ()
                 (let loop ((i N) (acc 0))
                   (if (= i 0) acc (loop (- i 1) (fast-add acc 1)))))))
       (t-old  (time-thunk (lambda ()
                 (let loop ((i N) (acc 0))
                   (if (= i 0) acc (loop (- i 1) (old-add acc 1))))))))
  (display "contract-overhead (2B.7 eta-elision) N=") (display N) (newline)
  (display "  uncontracted  ") (display t-raw)  (display " ms") (newline)
  (display "  fast-path     ") (display t-fast) (display " ms  [direct predicate calls]") (newline)
  (display "  old-style     ") (display t-old)  (display " ms  [__apply-domain dispatch]") (newline)
  (display "  fast/raw      ") (display (/ t-fast t-raw))  (display "x") (newline)
  (display "  old/fast      ") (display (/ t-old t-fast))  (display "x") (newline))
