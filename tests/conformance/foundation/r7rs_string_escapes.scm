(test-section "R7RS string escape sequences")

; --- standard escapes ---
(test-eqv "esc-n" 10 (char->integer (string-ref "\n" 0)))
(test-eqv "esc-t" 9  (char->integer (string-ref "\t" 0)))
(test-eqv "esc-r" 13 (char->integer (string-ref "\r" 0)))
(test-eqv "esc-a" 7  (char->integer (string-ref "\a" 0)))
(test-eqv "esc-b" 8  (char->integer (string-ref "\b" 0)))
(test-eqv "esc-v" 11 (char->integer (string-ref "\v" 0)))
(test-eqv "esc-f" 12 (char->integer (string-ref "\f" 0)))
(test-eqv "esc-0" 0  (char->integer (string-ref "\0" 0)))

; --- structural escapes ---
(test-equal "esc-quote"     "\""    "\"")
(test-equal "esc-backslash" "\\"    "\\")
(test-equal "esc-pipe"      "|"     "\|")

; --- hex codepoints with required semicolon ---
(test-eqv "esc-hex-A"       65   (char->integer (string-ref "\x41;" 0)))
(test-eqv "esc-hex-pi"      960  (char->integer (string-ref "\x3C0;" 0)))
(test-eqv "esc-hex-emoji"   128512 (char->integer (string-ref "\x1F600;" 0)))

; --- hex without trailing semicolon (we tolerate it) ---
(test-eqv "esc-hex-no-semi" 65 (char->integer (string-ref "\x41 next" 0)))

; --- R7RS line continuation: \<ws>?<newline><ws>?  produces no chars ---
(test-equal "esc-line-cont-basic" "ab"
  "a\
   b")
(test-equal "esc-line-cont-trailing-space" "ab"
  "a\
   b")
(test-equal "esc-line-cont-empty" ""
  "\
")

; --- mixed escapes in a single string ---
(test-equal "esc-mixed" "Hello\tWorld\n"
  "Hello\tWorld\n")

; --- combining hex + named escapes ---
(test-eqv "esc-string-len" 3
  (string-length "\x41;\x42;\x43;"))
(test-equal "esc-string-content" "ABC"
  "\x41;\x42;\x43;")
