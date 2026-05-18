; Conformance test for `(crab json)` — stdlib-modules iter 6.

(test-section "(crab json) — primitives")

(test-equal "parse integer" 42 (json-parse "42"))
(test-equal "parse string"  "x" (json-parse "\"x\""))
(test-equal "parse true"    #t  (json-parse "true"))
(test-equal "parse false"   #f  (json-parse "false"))
(test-equal "parse null is empty list" '() (json-parse "null"))

(test-equal "stringify integer"  "42" (json-stringify 42))
(test-equal "stringify string"   "\"x\"" (json-stringify "x"))
(test-equal "stringify bool"     "true" (json-stringify #t))

(test-section "(crab json) — arrays")

(test-equal "parse empty array" '() (json-parse "[]"))
(test-equal "parse number array" '(1 2 3) (json-parse "[1, 2, 3]"))
(test-equal "stringify list" "[1,2,3]" (json-stringify '(1 2 3)))

(test-section "(crab json) — objects")

; serde_json's default Map is alphabetically ordered, so the
; decoded alist is sorted regardless of input order.
(test-equal "parse simple object"
            '(("age" . 30) ("name" . "alice"))
            (json-parse "{\"name\":\"alice\",\"age\":30}"))

(test-equal "stringify alist (object)"
            "{\"k\":1}"
            (json-stringify '(("k" . 1))))

(test-section "(crab json) — round-trip")

; Keys are alphabetical after decode, so pick an input where the
; original order matches alphabetical (a, b, c) for a clean
; round-trip assertion.
(define __json-fixture__ "{\"a\":1,\"b\":[true,false],\"c\":\"hi\"}")
(test-equal "round-trip preserves structure (keys already alpha)"
            __json-fixture__
            (json-stringify (json-parse __json-fixture__)))
