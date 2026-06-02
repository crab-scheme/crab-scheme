; Conformance test for `(crab yaml)` — encoding/config batch.

(test-section "(crab yaml) — parse scalars + map")
(define __y__ (yaml-parse "host: localhost\nport: 8080\nenabled: true\n"))
(test-true "yaml-parse returns an alist (list)" (list? __y__))
(test-equal "string scalar" "localhost" (cdr (assoc "host" __y__)))
(test-equal "integer scalar parses to a fixnum" 8080 (cdr (assoc "port" __y__)))
(test-true "boolean scalar parses to #t" (cdr (assoc "enabled" __y__)))

(test-section "(crab yaml) — nested map")
(define __n__ (yaml-parse "server:\n  host: localhost\n  port: 8080\n"))
(define __srv__ (cdr (assoc "server" __n__)))
(test-equal "nested host" "localhost" (cdr (assoc "host" __srv__)))
(test-equal "nested port" 8080 (cdr (assoc "port" __srv__)))

(test-section "(crab yaml) — sequence")
(define __seq__ (yaml-parse "- a\n- b\n- c\n"))
(test-true "sequence parses to a list" (list? __seq__))
(test-equal "sequence length" 3 (length __seq__))
(test-equal "first element" "a" (car __seq__))

(test-section "(crab yaml) — stringify round-trip")
(define __doc__ (yaml-parse "name: prod\ncount: 3\n"))
(define __s__ (yaml-stringify __doc__))
(test-true "yaml-stringify returns a string" (string? __s__))
(define __rt__ (yaml-parse __s__))
(test-equal "round-trip preserves name" "prod" (cdr (assoc "name" __rt__)))
(test-equal "round-trip preserves count" 3 (cdr (assoc "count" __rt__)))
