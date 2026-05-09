(test-section "library declaration validation: name shape, export list")

; --- a well-formed library still works (regression) ---
(library (foo bar)
  (export quux)
  (import (rnrs base))
  (define quux 42))
(test-eqv "well-formed-library-binds" 42 quux)

; --- empty export list is fine ---
(library (no-exports)
  (export)
  (import (rnrs base))
  (define internal 1))
(test-eqv "empty-export-runs" 1 internal)

; --- library with a version in the name parses; version stripped ---
(library (with-version (1 0))
  (export ver-bound)
  (import (rnrs base))
  (define ver-bound 'ok))
(test-equal "lib-with-version" 'ok ver-bound)

; --- library with R6RS multi-segment name (rnrs base) shape parses ---
(library (my-rnrs base extras)
  (export multi)
  (import (rnrs base))
  (define multi (+ 1 2)))
(test-eqv "multi-segment-name" 3 multi)

; --- library import inside library body: rename works ---
(library (with-rename)
  (export rcons)
  (import (rename (rnrs base) (cons rcons)))
  ;; rcons should be available from the import line above.
  )
(test-equal "library-rename-imports" '(1 . 2) (rcons 1 2))

; --- imports run BEFORE body defines, so renamed bindings are usable ---
(library (rename-then-define)
  (export use-renamed)
  (import (rename (rnrs base) (+ plus2)))
  (define (use-renamed x y) (plus2 x y)))
(test-eqv "renamed-then-defined" 12 (use-renamed 5 7))
