; Conformance test for `(crab csv)` — stdlib-modules iter 6.

(test-section "(crab csv) — parse")

(test-equal "parse two rows"
            '(("a" "b" "c") ("1" "2" "3"))
            (csv-parse "a,b,c\n1,2,3\n"))

(test-equal "parse quoted field with comma"
            '(("a,b" "c"))
            (csv-parse "\"a,b\",c\n"))

(test-equal "parse empty input"
            '()
            (csv-parse ""))

(test-section "(crab csv) — write")

(test-equal "write two rows"
            "a,b,c\n1,2,3\n"
            (csv-write '(("a" "b" "c") ("1" "2" "3"))))

(test-equal "write quotes field containing comma"
            "\"a,b\",c\n"
            (csv-write '(("a,b" "c"))))

(test-equal "write round-trip"
            "x,y\n1,2\n"
            (csv-write (csv-parse "x,y\n1,2\n")))
