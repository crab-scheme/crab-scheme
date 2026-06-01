;;; (crab mock) — mock / stub procedures with call recording.
;;;
;;; A bundled Scheme library (global at startup). A mock is a callable
;;; procedure that records its calls and returns a configured value; the
;;; recorded calls can be inspected and verified afterward.
;;;
;;; A mock dispatches on a private sentinel as its first argument for
;;; queries/configuration, so ordinary calls (which never see the
;;; sentinel) are recorded transparently.

;; Unique query sentinel — a real call can't accidentally match it.
(define mock:q (list 'mock-query))

;; Global monotonic tick so call order across mocks can be compared.
(define mock:tick 0)
(define (mock:next-tick)
  (set! mock:tick (+ mock:tick 1))
  mock:tick)

;; (make-mock) — a fresh mock procedure (returns #f until configured).
(define (make-mock)
  (let ((calls '())      ; reverse-chronological list of (tick . args)
        (mode 'fixed)    ; 'fixed | 'seq | 'impl
        (val #f)
        (seq '())
        (impl #f))
    (lambda args
      (if (and (pair? args) (eq? (car args) mock:q))
          ;; ----- query / configure -----
          (let ((op (cadr args)))
            (cond
              ((eq? op 'calls) (map cdr (reverse calls)))
              ((eq? op 'count) (length calls))
              ((eq? op 'first-tick) (if (null? calls) #f (car (car (reverse calls)))))
              ((eq? op 'set-return!) (set! mode 'fixed) (set! val (caddr args)))
              ((eq? op 'set-seq!) (set! mode 'seq) (set! seq (caddr args)))
              ((eq? op 'set-impl!) (set! mode 'impl) (set! impl (caddr args)))
              ((eq? op 'reset!) (set! calls '()))
              (else (error "mock: unknown query" op))))
          ;; ----- ordinary call: record + return -----
          (begin
            (set! calls (cons (cons (mock:next-tick) args) calls))
            (cond
              ((eq? mode 'impl) (apply impl args))
              ((eq? mode 'seq)
               (if (null? seq)
                   #f
                   (let ((v (car seq)))
                     ;; last element repeats once the sequence is exhausted
                     (when (pair? (cdr seq)) (set! seq (cdr seq)))
                     v)))
              (else val)))))))

;; ---- configuration ----
(define (mock-returns! m v) (m mock:q 'set-return! v))
(define (mock-returns-seq! m . vs) (m mock:q 'set-seq! vs))
(define (mock-impl! m proc) (m mock:q 'set-impl! proc))
(define (mock-reset! m) (m mock:q 'reset!))

;; ---- inspection ----
(define (mock-calls m) (m mock:q 'calls))
(define (mock-call-count m) (m mock:q 'count))
(define (mock-called? m) (> (mock-call-count m) 0))
(define (mock-nth-call m n) (list-ref (mock-calls m) n))

;; ---- argument matchers (for mock-called-with?) ----
;; `arg-any` (not `any` — that's SRFI-1's higher-order predicate) and
;; `arg-that` build argument matchers for `mock-called-with?`.
(define mock:any-tag (list 'mock-any))
(define (arg-any) (cons mock:any-tag #f))
(define (arg-that pred) (cons mock:any-tag pred))
(define (mock:matcher? x) (and (pair? x) (eq? (car x) mock:any-tag)))
(define (mock:arg-matches? expected actual)
  (if (mock:matcher? expected)
      (if (cdr expected) ((cdr expected) actual) #t) ; arg-that pred, or any
      (equal? expected actual)))
(define (mock:args-match? expected actual)
  (cond ((and (null? expected) (null? actual)) #t)
        ((or (null? expected) (null? actual)) #f)
        ((mock:arg-matches? (car expected) (car actual))
         (mock:args-match? (cdr expected) (cdr actual)))
        (else #f)))

;; (mock-called-with? m arg …) — was the mock ever called with arguments
;; matching `arg …`? Each `arg` may be a literal, `(arg-any)`, or
;; `(arg-that pred)`.
(define (mock-called-with? m . expected)
  (let loop ((cs (mock-calls m)))
    (cond ((null? cs) #f)
          ((mock:args-match? expected (car cs)) #t)
          (else (loop (cdr cs))))))

;; (mock-called-before? a b) — did mock `a`'s first call precede `b`'s?
(define (mock-called-before? a b)
  (let ((ta (a mock:q 'first-tick))
        (tb (b mock:q 'first-tick)))
    (and ta tb (< ta tb))))
