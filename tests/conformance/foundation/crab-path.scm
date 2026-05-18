; Conformance test for `(crab path)` — stdlib-modules iter 2.
; Pure path manipulation; no filesystem access.

(test-section "(crab path)")

; path-join
(test-equal "join two segments"
            "/etc/nginx"
            (path-join "/etc" "nginx"))
(test-equal "join three segments"
            "/etc/nginx/sites-enabled"
            (path-join "/etc" "nginx" "sites-enabled"))
(test-equal "join single segment passes through"
            "/etc"
            (path-join "/etc"))

; path-basename
(test-equal "basename absolute"
            "syslog.1"
            (path-basename "/var/log/syslog.1"))
(test-equal "basename bare filename"
            "file.txt"
            (path-basename "file.txt"))

; path-dirname
(test-equal "dirname absolute"
            "/var/log"
            (path-dirname "/var/log/syslog.1"))
(test-equal "dirname bare filename"
            ""
            (path-dirname "file.txt"))

; path-extension
(test-equal "extension simple"
            "txt"
            (path-extension "file.txt"))
(test-equal "extension absent"
            ""
            (path-extension "Makefile"))
(test-equal "extension multi-dot returns last"
            "gz"
            (path-extension "archive.tar.gz"))

; path-stem
(test-equal "stem simple"
            "file"
            (path-stem "file.txt"))
(test-equal "stem with directory"
            "syslog"
            (path-stem "/var/log/syslog.1"))

; path-is-absolute?
(test-true  "absolute unix path" (path-is-absolute? "/etc"))
(test-false "relative path"       (path-is-absolute? "etc/nginx"))
(test-false "empty path"          (path-is-absolute? ""))

; path-with-extension
(test-equal "with-extension replace"
            "report.md"
            (path-with-extension "report.txt" "md"))
(test-equal "with-extension add"
            "Makefile.bak"
            (path-with-extension "Makefile" "bak"))
(test-equal "with-extension strip"
            "report"
            (path-with-extension "report.txt" ""))

; path-components
(test-equal "components absolute"
            '("/" "usr" "local" "bin")
            (path-components "/usr/local/bin"))
