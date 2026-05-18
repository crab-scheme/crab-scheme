; Conformance test for `(crab format)` — stdlib-modules iter 4.

(test-section "(crab format) — directives")

(test-equal "no directives passes through"
            "hello"
            (format-string "hello"))

(test-equal "~a display string"
            "hello world"
            (format-string "~a ~a" "hello" "world"))

(test-equal "~s write string"
            "the value is \"hi\""
            (format-string "the value is ~s" "hi"))

(test-equal "~d decimal"
            "n = 42"
            (format-string "n = ~d" 42))

(test-equal "~x hex lower"
            "0xff"
            (format-string "0x~x" 255))

(test-equal "~X hex upper"
            "0xFF"
            (format-string "0x~X" 255))

(test-equal "~% newline"
            "line1\nline2"
            (format-string "line1~%line2"))

(test-equal "~~ literal tilde"
            "100~~"
            (format-string "~d~~~~" 100))

(test-equal "booleans render"
            "got #t and #f"
            (format-string "got ~a and ~a" #t #f))

(test-equal "characters render display"
            "a"
            (format-string "~a" #\a))
