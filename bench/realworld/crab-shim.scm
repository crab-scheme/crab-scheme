;; CrabScheme shim mirroring chez-shim's shape: run the thunk once,
;; write the result. Used by check-result-vs-chez.sh for correctness
;; gating (the real harness/runner.sh uses lib/harness.scm).
(define (realworld-bench name params thunk)
  (let ((result (thunk)))
    (write result)
    (newline)))
