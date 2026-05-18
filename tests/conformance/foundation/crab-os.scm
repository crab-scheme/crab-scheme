; Conformance test for `(crab os)` — stdlib-modules iter 3.

(test-section "(crab os) — environment")

; A name we control so the assertion is hermetic. Use a name that
; almost certainly doesn't exist in the embedder's environment.
(define __crab-os-test-name__ "__CRAB_OS_CONFORMANCE_VAR__")

(test-false "missing env returns #f"
            (get-env __crab-os-test-name__))

(set-env! __crab-os-test-name__ "iter-3-value")
(test-equal "get-env after set"
            "iter-3-value"
            (get-env __crab-os-test-name__))

(unset-env! __crab-os-test-name__)
(test-false "unset-env! removes it"
            (get-env __crab-os-test-name__))

; env-vars returns a non-empty list when PATH or HOME or similar is set.
(test-true "env-vars returns a non-empty list when shell vars exist"
           (let loop ((rest (env-vars)) (any-pair #f))
             (cond ((null? rest) any-pair)
                   ((pair? (car rest)) #t)
                   (else (loop (cdr rest) #f)))))

(test-section "(crab os) — identity + platform")

(test-true "process-id is a positive fixnum" (> (process-id) 0))
(test-true "parent-process-id is a non-negative fixnum"
           (>= (parent-process-id) 0))

(test-true "hostname returns a non-empty string"
           (> (string-length (hostname)) 0))

; platform/architecture are documented to return strings like "linux"
; or "macos" / "x86_64" or "aarch64". Don't assert specific values
; because that varies per CI host.
(test-true "platform is a non-empty string"
           (and (string? (platform)) (> (string-length (platform)) 0)))
(test-true "architecture is a non-empty string"
           (and (string? (architecture)) (> (string-length (architecture)) 0)))

(test-section "(crab os) — working directory")

(define __crab-os-cwd-before__ (current-directory))
(test-true "current-directory returns a non-empty string"
           (and (string? __crab-os-cwd-before__)
                (> (string-length __crab-os-cwd-before__) 0)))

; Round-trip cd: into / and back.
(change-directory "/")
(test-equal "change-directory persists across calls"
            "/"
            (current-directory))
(change-directory __crab-os-cwd-before__)
(test-equal "change-directory back round-trips"
            __crab-os-cwd-before__
            (current-directory))
