;; A metacircular Scheme evaluator.
;;
;; Implements a Scheme subset (numbers, booleans, symbols, lists, lambdas,
;; if, define, set!, begin, quote, let, primitive ops) and runs a small
;; test program through it. Demonstrates closures + recursion + pattern
;; dispatch on both the tree-walker and the bytecode VM.
;;
;;   $ crabscheme run examples/metacircular.scm
;;   $ crabscheme --tier vm run examples/metacircular.scm

;; ---------- environments (alist of (name . value)) ----------

(define (env-empty) '())
(define (env-extend env names vals)
  (if (null? names)
      env
      (cons (cons (car names) (car vals))
            (env-extend env (cdr names) (cdr vals)))))
(define (env-lookup env name)
  (cond
    ((null? env) (error "unbound" name))
    ((eq? (car (car env)) name) (cdr (car env)))
    (else (env-lookup (cdr env) name))))
(define (env-set! env name val)
  (cond
    ((null? env) (error "unbound (set!)" name))
    ((eq? (car (car env)) name)
     (set-cdr! (car env) val))
    (else (env-set! (cdr env) name val))))

;; ---------- closures ----------

(define (make-closure params body env)
  (list 'closure params body env))
(define (closure? v)
  (and (pair? v) (eq? (car v) 'closure)))
(define (closure-params c) (car (cdr c)))
(define (closure-body c)   (car (cdr (cdr c))))
(define (closure-env c)    (car (cdr (cdr (cdr c)))))

;; ---------- primitives table ----------

(define primitives
  (list
    (cons '+ +)
    (cons '- -)
    (cons '* *)
    (cons '/ /)
    (cons '< <)
    (cons '> >)
    (cons '= =)
    (cons 'cons cons)
    (cons 'car  car)
    (cons 'cdr  cdr)
    (cons 'list list)
    (cons 'null? null?)
    (cons 'pair? pair?)
    (cons 'eq?  eq?)
    (cons 'not  not)))

(define (primitive? v) (procedure? v))

;; ---------- the evaluator ----------

(define (mc-eval expr env)
  (cond
    ((number? expr)  expr)
    ((boolean? expr) expr)
    ((string? expr)  expr)
    ((symbol? expr)  (env-lookup env expr))
    ((pair? expr)
     (let ((head (car expr))
           (args (cdr expr)))
       (cond
         ((eq? head 'quote)  (car args))
         ((eq? head 'if)
          (if (mc-eval (car args) env)
              (mc-eval (car (cdr args)) env)
              (mc-eval (car (cdr (cdr args))) env)))
         ((eq? head 'lambda)
          (let ((params (car args))
                (body   (cdr args)))
            ;; Multi-expression bodies are wrapped in begin so a single
            ;; expression slot is enough on the closure.
            (make-closure params
                          (if (null? (cdr body))
                              (car body)
                              (cons 'begin body))
                          env)))
         ((eq? head 'define)
          (let ((name (car args))
                (val  (mc-eval (car (cdr args)) env)))
            (set-cdr! env (cons (car env) (cdr env)))
            (set-car! env (cons name val))
            'ok))
         ((eq? head 'set!)
          (let ((name (car args))
                (val  (mc-eval (car (cdr args)) env)))
            (env-set! env name val)
            'ok))
         ((eq? head 'begin)
          (mc-eval-begin args env))
         ((eq? head 'let)
          (let ((bindings (car args))
                (body     (car (cdr args))))
            (let ((names (map car bindings))
                  (vals  (map (lambda (b) (mc-eval (car (cdr b)) env)) bindings)))
              (mc-eval body (env-extend env names vals)))))
         (else
          ;; application
          (let ((proc (mc-eval head env))
                (vals (map (lambda (a) (mc-eval a env)) args)))
            (mc-apply proc vals))))))
    (else (error "bad expression" expr))))

(define (mc-eval-begin exprs env)
  (cond
    ((null? exprs) 'unspecified)
    ((null? (cdr exprs)) (mc-eval (car exprs) env))
    (else (mc-eval (car exprs) env)
          (mc-eval-begin (cdr exprs) env))))

(define (mc-apply proc args)
  (cond
    ((closure? proc)
     (mc-eval (closure-body proc)
              (env-extend (closure-env proc)
                          (closure-params proc)
                          args)))
    ((primitive? proc) (apply proc args))
    (else (error "not a procedure" proc))))

;; ---------- driver ----------

(define (mc-run expr)
  (mc-eval expr primitives))

;; ---------- demo program ----------

;; Compute factorial via the metacircular interpreter.
(define program
  '(begin
     (define fact
       (lambda (n)
         (if (= n 0)
             1
             (* n (fact (- n 1))))))
     (fact 10)))

(display "metacircular: ") (display (mc-run program)) (newline)

;; Compute sum 1..100 via let + recursion through the metacircular interpreter.
(define program2
  '(let ((loop 0))
     (let ((go (lambda (i acc)
                 (if (> i 100)
                     acc
                     (go (+ i 1) (+ acc i))))))
       (go 1 0))))
;; (Note: the metacircular evaluator above doesn't implement letrec, so use
;; Y-combinator-style self-passing or a top-level define.)

(define program3
  '(begin
     (define sum-up-to
       (lambda (n)
         (define go
           (lambda (i acc)
             (if (> i n)
                 acc
                 (go (+ i 1) (+ acc i)))))
         (go 1 0)))
     (sum-up-to 100)))

(display "metacircular sum 1..100: ") (display (mc-run program3)) (newline)

;; Closures: counter
(define program4
  '(begin
     (define make-counter
       (lambda ()
         (define n 0)
         (lambda ()
           (set! n (+ n 1))
           n)))
     (define c (make-counter))
     (c) (c) (c)))

(display "metacircular counter (3 calls): ") (display (mc-run program4)) (newline)
