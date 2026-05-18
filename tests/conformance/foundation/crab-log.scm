; Conformance test for `(crab log)` — stdlib-modules iter 8.

(test-section "(crab log) — level control")

(log-set-level! "info")
(test-equal "set then read level" "info" (log-current-level))

(log-set-level! "debug")
(test-equal "set debug" "debug" (log-current-level))

(log-set-level! "off")
(test-equal "set off" "off" (log-current-level))

;; restore to error so the next assertions don't flood test output
(log-set-level! "error")

(test-section "(crab log) — emit")

;; All emit procs return unspecified; we just smoke-test they don't error.
(log-error "smoke" "test" 1 2 3)
(log-warn "below threshold")    ; suppressed at level=error; no output
(log-info "below threshold")
(log-debug "below threshold")
(log-trace "below threshold")
(test-true "log-error returned without raising" #t)

(test-section "(crab log) — invalid level rejected")

(test-true "bogus level raises"
           (guard (e (#t #t))
             (log-set-level! "nope")
             #f))

;; restore default for downstream tests
(log-set-level! "info")
