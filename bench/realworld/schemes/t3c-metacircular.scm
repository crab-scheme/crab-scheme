; T3-C — SICP-style metacircular evaluator.
;
; Implements a minimal Scheme-in-Scheme: eval/apply over a
; subset (define, lambda, if, set!, quote, primitive apps), then
; runs a workload program through it. The "workload" is a
; quicksort over a fixed-size list of integers (descending list
; sorted ascending, so all elements move).
;
; One iter = N invocations of the eval'd quicksort. Each invocation
; reuses the eval'd procedure but allocates fresh cons cells per
; recursive call, exercising closure + pair allocation under
; interpretation rather than native dispatch.
;
; The metacircular layer adds ~10-30x overhead over a native call,
; so each iter is dominated by interpreter dispatch — useful for
; detecting regressions in the host VM's per-call cost.

(define qsort-iters 8)
(define input-size 200)

; --- Environment: alist-of-bindings, with shadowing. ----

(define (env-extend names vals parent)
  (cons (map cons names vals) parent))

(define (env-lookup env name)
  (cond
    ((null? env) (error 'env-lookup "unbound" name))
    (else
      (let ((found (assq name (car env))))
        (if found
            (cdr found)
            (env-lookup (cdr env) name))))))

(define (env-set! env name val)
  (cond
    ((null? env) (error 'env-set! "unbound" name))
    (else
      (let ((found (assq name (car env))))
        (if found
            (set-cdr! found val)
            (env-set! (cdr env) name val))))))

(define (env-define! env name val)
  (let ((frame (car env)))
    (set-car! env (cons (cons name val) frame))))

; --- Eval / apply ----

; Tag for user-defined procedures: ('proc params body env).
(define (make-proc params body env) (list 'proc params body env))
(define (proc? p) (and (pair? p) (eq? (car p) 'proc)))
(define (proc-params p) (car (cdr p)))
(define (proc-body p) (car (cdr (cdr p))))
(define (proc-env p) (car (cdr (cdr (cdr p)))))

(define (mc-eval expr env)
  (cond
    ((number? expr) expr)
    ((boolean? expr) expr)
    ((null? expr) '())
    ((symbol? expr) (env-lookup env expr))
    ((pair? expr)
     (let ((head (car expr)))
       (cond
         ((eq? head 'quote) (car (cdr expr)))
         ((eq? head 'if)
          (let ((c (mc-eval (car (cdr expr)) env)))
            (if c
                (mc-eval (car (cdr (cdr expr))) env)
                (mc-eval (car (cdr (cdr (cdr expr)))) env))))
         ((eq? head 'lambda)
          (make-proc (car (cdr expr)) (car (cdr (cdr expr))) env))
         ((eq? head 'define)
          (env-define! env (car (cdr expr))
                       (mc-eval (car (cdr (cdr expr))) env)))
         ((eq? head 'set!)
          (env-set! env (car (cdr expr))
                    (mc-eval (car (cdr (cdr expr))) env)))
         ((eq? head 'begin)
          (mc-eval-body (cdr expr) env))
         (else
          (mc-apply (mc-eval head env)
                    (mc-eval-args (cdr expr) env))))))
    (else expr)))

(define (mc-eval-args args env)
  (if (null? args)
      '()
      (cons (mc-eval (car args) env)
            (mc-eval-args (cdr args) env))))

(define (mc-eval-body body env)
  (cond
    ((null? body) #f)
    ((null? (cdr body)) (mc-eval (car body) env))
    (else (mc-eval (car body) env)
          (mc-eval-body (cdr body) env))))

(define (mc-apply proc args)
  (cond
    ((proc? proc)
     (let ((new-env (env-extend (proc-params proc) args (proc-env proc))))
       (mc-eval (proc-body proc) new-env)))
    ((procedure? proc) (apply proc args))
    (else (error 'mc-apply "not a procedure" proc))))

; --- The workload program (will be eval'd by mc-eval). ----

(define workload-program
  '(define qsort
     (lambda (lst)
       (if (if (null? lst) #t (null? (cdr lst)))
           lst
           (let-helper qsort lst)))))

; let-helper inlined as a series of operations; mc-eval doesn't
; have `let`, so we phrase the pivot-and-partition manually.
(define workload-let-helper
  '(define let-helper
     (lambda (q lst)
       (concat
         (q (filter-lt (cdr lst) (car lst)))
         (cons (car lst)
               (q (filter-ge (cdr lst) (car lst))))))))

(define workload-helpers
  '(begin
     (define filter-lt
       (lambda (xs p)
         (if (null? xs) '()
             (if (< (car xs) p)
                 (cons (car xs) (filter-lt (cdr xs) p))
                 (filter-lt (cdr xs) p)))))
     (define filter-ge
       (lambda (xs p)
         (if (null? xs) '()
             (if (< (car xs) p)
                 (filter-ge (cdr xs) p)
                 (cons (car xs) (filter-ge (cdr xs) p))))))
     (define concat
       (lambda (a b)
         (if (null? a) b
             (cons (car a) (concat (cdr a) b)))))))

; Build the descending input list once per iter.
(define (make-input n)
  (let loop ((i 0) (acc '()))
    (if (= i n) acc
        (loop (+ i 1) (cons i acc)))))

(define base-env
  (env-extend
    '(null? cdr car cons + - < pair?)
    (list null? cdr car cons + - < pair?)
    '(())))

(define (setup-evaluator!)
  ; Run the helper + workload defines in the base env so each iter
  ; can fish out qsort directly.
  (mc-eval workload-helpers base-env)
  (mc-eval workload-let-helper base-env)
  (mc-eval workload-program base-env))

(setup-evaluator!)

(define qsort-proc (env-lookup base-env 'qsort))

(define (one-iter)
  (let loop ((i 0) (acc 0))
    (if (= i qsort-iters)
        acc
        (let ((result (mc-apply qsort-proc (list (make-input input-size)))))
          (loop (+ i 1) (+ acc (length result)))))))

(realworld-bench
  "t3c-metacircular"
  (list (cons (quote qsort-iters) qsort-iters)
        (cons (quote input-size) input-size))
  (lambda () (one-iter)))
