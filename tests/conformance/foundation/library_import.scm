(test-section "library/import prologue (M9 stub)")

; A bare `(import ...)` form is a no-op — every binding is already global
; at this milestone, so the import is just a recognition gate.
(import (rnrs base (6)))
(test-eqv "import-then-call" 6 (+ 1 2 3))
(test-eqv "import-then-multi-arg" 24 (* 2 3 4))

; Multiple imports in one form work too.
(import (rnrs base (6)) (rnrs lists (6)))
(test-eqv "multi-import-length" 3 (length (list 'a 'b 'c)))

; A library body splices in as if its forms were top-level.
(library (test/sq (1 0))
  (export sq)
  (import (rnrs))
  (define (sq x) (* x x))
  (test-eqv "lib-body-defines-and-uses" 49 (sq 7)))

; Library forms can include record types, condition types, etc.
(library (test/rec)
  (export)
  (import (rnrs))
  (define-record-type tagged (fields name value))
  (define t (make-tagged 'pi 3.14159))
  (test-equal "lib-record-name" 'pi (tagged-name t))
  (test-eqv   "lib-record-value" 3.14159 (tagged-value t)))

; Empty body is fine.
(library (test/empty) (export) (import (rnrs)))
(test-eqv "after-empty-lib" 1 1)

; Nested library invocations.
(library (test/outer)
  (export)
  (import (rnrs))
  (library (test/inner)
    (export)
    (import (rnrs))
    (define inner-x 10)
    (test-eqv "inner-lib-runs" 10 inner-x))
  ; the inner library's binding is also visible because we don't namespace.
  (test-eqv "outer-sees-inner" 10 inner-x))
