; Conformance test for `(crab ini)` — encoding/config batch.

(test-section "(crab ini) — parse + ref")

(define __cfg__
  (ini-parse "; a comment\n[server]\nhost = localhost\nport = 8080\n\n[db]\nname = prod\n"))

(test-equal "ini-ref reads a value" "localhost" (ini-ref __cfg__ "server" "host"))
(test-equal "ini-ref reads a second key" "8080" (ini-ref __cfg__ "server" "port"))
(test-equal "ini-ref reads across sections" "prod" (ini-ref __cfg__ "db" "name"))
(test-false "ini-ref missing key returns #f by default" (ini-ref __cfg__ "server" "nope"))
(test-equal "ini-ref missing key returns the supplied default" "x"
            (ini-ref __cfg__ "server" "nope" "x"))
(test-false "ini-ref missing section returns #f" (ini-ref __cfg__ "nosuch" "host"))

(test-section "(crab ini) — comments + blank lines skipped")
(define __cfg2__ (ini-parse "# hash comment\n; semi comment\n\n[s]\nk = v\n"))
(test-equal "comment + blank lines ignored" "v" (ini-ref __cfg2__ "s" "k"))

(test-section "(crab ini) — keys before any section")
(define __cfg3__ (ini-parse "global = 1\n[s]\nk = v\n"))
(test-equal "key before [section] lives under the empty-string section" "1"
            (ini-ref __cfg3__ "" "global"))

(test-section "(crab ini) — alist shape")
(test-true "ini-parse returns a list" (list? __cfg__))
(test-true "first section is a (name . pairs) pair" (pair? (car __cfg__)))
(test-equal "first section name" "server" (car (car __cfg__)))

(test-section "(crab ini) — stringify round-trip")
(define __round__ (ini-parse (ini-stringify __cfg__)))
(test-equal "round-trip preserves host" "localhost" (ini-ref __round__ "server" "host"))
(test-equal "round-trip preserves db name" "prod" (ini-ref __round__ "db" "name"))
