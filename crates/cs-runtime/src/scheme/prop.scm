;;; (crab prop) — property-based testing, fuzzing, and model-based checks.
;;;
;;; A bundled Scheme library (global at startup). The answer to
;;; QuickCheck / Hypothesis. A generator is `#(__gen__ produce shrink)`:
;;; `produce` is a thunk returning a random value, `shrink` maps a value
;;; to a list of smaller candidates (toward a minimal counterexample).

(define prop:tag '__gen__)
(define (gen:make produce shrink) (vector prop:tag produce shrink))
(define (gen? x)
  (and (vector? x) (= (vector-length x) 3) (eq? (vector-ref x 0) prop:tag)))
(define (generate g) ((vector-ref g 1)))
(define (gen:shrink-of g v) ((vector-ref g 2) v))

(define (gen:iota n)
  (let loop ((i 0) (acc '())) (if (>= i n) (reverse acc) (loop (+ i 1) (cons i acc)))))
(define (gen:take lst n)
  (if (or (= n 0) (null? lst)) '() (cons (car lst) (gen:take (cdr lst) (- n 1)))))

;; ---- generators ----

;; Shrink an integer toward 0 (0, then halfway).
(define (gen:shrink-int n)
  (cond ((= n 0) '())
        ((= (quotient n 2) 0) (list 0))
        (else (list 0 (quotient n 2)))))

;; (gen-int) | (gen-int lo hi) — uniform integer in [lo, hi).
(define (gen-int . bounds)
  (let ((lo (if (pair? bounds) (car bounds) -1000))
        (hi (if (and (pair? bounds) (pair? (cdr bounds))) (cadr bounds) 1000)))
    (gen:make (lambda () (+ lo (random-integer (- hi lo))))
              gen:shrink-int)))

(define (gen-bool)
  (gen:make (lambda () (= 0 (random-integer 2)))
            (lambda (b) (if b (list #f) '()))))

(define (gen-char)
  (gen:make (lambda () (integer->char (+ 97 (random-integer 26))))
            (lambda (c) (if (char=? c #\a) '() (list #\a)))))

(define (gen-string . bounds)
  (let ((maxlen (if (pair? bounds) (car bounds) 16)))
    (gen:make
     (lambda ()
       (let ((len (random-integer (+ maxlen 1))))
         (list->string
          (map (lambda (_) (integer->char (+ 97 (random-integer 26)))) (gen:iota len)))))
     (lambda (s)
       (if (= 0 (string-length s))
           '()
           (list "" (substring s 0 (quotient (string-length s) 2))))))))

(define (gen-list-of g . bounds)
  (let ((maxlen (if (pair? bounds) (car bounds) 16)))
    (gen:make
     (lambda ()
       (let ((len (random-integer (+ maxlen 1))))
         (map (lambda (_) (generate g)) (gen:iota len))))
     (lambda (lst)
       (if (null? lst) '() (list '() (gen:take lst (quotient (length lst) 2))))))))

;; (gen-one-of v …) — pick one of fixed values (no shrinking).
(define (gen-one-of . vals)
  (gen:make (lambda () (random-choice vals)) (lambda (v) '())))

;; (gen-choose g …) — pick one of several generators.
(define (gen-choose . gens)
  (gen:make (lambda () (generate (random-choice gens))) (lambda (v) '())))

;; (gen-map f g) — transform generated values (shrinking is dropped).
(define (gen-map f g)
  (gen:make (lambda () (f (generate g))) (lambda (v) '())))

;; (gen-tuple g …) — a list with one value per generator.
(define (gen-tuple . gens)
  (gen:make (lambda () (map generate gens)) (lambda (v) '())))

;; ---- property checking ----

;; True iff `property` holds for `v` (a raised error counts as failure,
;; so `check` doubles as a crash finder).
(define (prop:holds? property v)
  (guard (e (#t #f)) (if (property v) #t #f)))

;; Greedily shrink a failing value to a (locally) minimal one.
(define (prop:shrink property gen v)
  (let loop ((v v))
    (let scan ((cands (gen:shrink-of gen v)))
      (cond ((null? cands) v)
            ((not (prop:holds? property (car cands))) (loop (car cands)))
            (else (scan (cdr cands)))))))

;; (check property gen [count]) — run `property` over `count` (default
;; 100) generated values; on failure shrink and raise with the minimal
;; counterexample. (`for-all` is a built-in, hence `check`.)
(define (check property gen . opts)
  (let ((count (if (pair? opts) (car opts) 100)))
    (let loop ((i 0))
      (if (>= i count)
          #t
          (let ((v (generate gen)))
            (if (prop:holds? property v)
                (loop (+ i 1))
                (error (string-append "property failed for input: "
                                      (expect:->str (prop:shrink property gen v))))))))))

;; (fuzz fn gen [count]) — call `(fn value)` on random inputs; fail with
;; the shrunk input if any call raises.
(define (fuzz fn gen . opts)
  (apply check (lambda (v) (fn v) #t) gen opts))

;; ---- model-based / stateful testing ----

;; (check-model init-model make-real gen-op step-model step-real ok?
;;              [trials [steps]])
;; For each of `trials` trials: build a fresh real system, then apply
;; `steps` random operations to both a pure model and the real system,
;; checking `(ok? model real)` after each. Reports the operation sequence
;; on the first divergence.
(define (check-model init-model make-real gen-op step-model step-real ok? . opts)
  (let ((trials (if (pair? opts) (car opts) 50))
        (steps (if (and (pair? opts) (pair? (cdr opts))) (cadr opts) 20)))
    (let trial ((tr 0))
      (if (>= tr trials)
          #t
          (let ((real (make-real)))
            (let step ((s 0) (model init-model) (ops '()))
              (if (>= s steps)
                  (trial (+ tr 1))
                  (let* ((op (generate gen-op))
                         (ops2 (cons op ops))
                         (model2 (step-model model op)))
                    (step-real real op)
                    (if (ok? model2 real)
                        (step (+ s 1) model2 ops2)
                        (error (string-append "model check failed after ops: "
                                              (expect:->str (reverse ops2)))))))))))))
