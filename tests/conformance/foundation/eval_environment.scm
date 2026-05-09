(test-section "eval + environment / interaction-environment")

; eval with explicit environment argument (R6RS shape).
(test-eqv "eval-with-environment"
  6
  (eval '(+ 1 2 3) (environment '(rnrs base (6)))))

; eval with interaction-environment.
(test-eqv "eval-with-interaction-environment"
  20
  (eval '(* 4 5) (interaction-environment)))

; environment accepts any number of import-spec args at this milestone.
(test-eqv "environment-multi-imports"
  3
  (eval '(- 10 7) (environment '(rnrs base (6)) '(rnrs lists (6)))))

; eval still works without environment (foundation extension).
(test-eqv "eval-no-env"
  42
  (eval '(* 6 7)))

; Top-level definitions are visible to eval (since every binding is global).
(define top-level-x 99)
(test-eqv "eval-sees-top-level"
  99
  (eval 'top-level-x (interaction-environment)))

; eval can build new top-level definitions.
(eval '(define from-eval 123) (interaction-environment))
(test-eqv "eval-defines-top-level" 123 from-eval)

; environment value is reusable.
(define env (environment '(rnrs)))
(test-eqv "env-reuse-1" 4 (eval '(+ 2 2) env))
(test-eqv "env-reuse-2" 9 (eval '(* 3 3) env))
