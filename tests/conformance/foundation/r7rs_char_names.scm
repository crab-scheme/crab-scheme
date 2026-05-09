(test-section "R7RS char names + hex literal forms")

; --- standard R7RS named characters ---
(test-eqv "char-alarm"     7   (char->integer #\alarm))
(test-eqv "char-backspace" 8   (char->integer #\backspace))
(test-eqv "char-delete"    127 (char->integer #\delete))
(test-eqv "char-escape"    27  (char->integer #\escape))

; --- R6RS-named char names still work ---
(test-eqv "char-space"   32 (char->integer #\space))
(test-eqv "char-newline" 10 (char->integer #\newline))
(test-eqv "char-tab"     9  (char->integer #\tab))
(test-eqv "char-return"  13 (char->integer #\return))
(test-eqv "char-nul"     0  (char->integer #\nul))
(test-eqv "char-null"    0  (char->integer #\null))

; --- hex character literals: #\xHH ---
(test-eqv "char-hex-A"      65    (char->integer #\x41))    ; 'A'
(test-eqv "char-hex-lower"  97    (char->integer #\x61))    ; 'a'
(test-eqv "char-hex-zero"   0     (char->integer #\x0))
(test-eqv "char-hex-7f"     127   (char->integer #\x7F))

; --- hex chars over 8 bits (Unicode codepoints) ---
(test-eqv "char-hex-pi"     960   (char->integer #\x3C0))   ; π
(test-eqv "char-hex-emoji"  128512 (char->integer #\x1F600)) ; 😀

; --- single-char literal ---
(test-eqv "char-A"  65 (char->integer #\A))
(test-eqv "char-z"  122 (char->integer #\z))
(test-eqv "char-0"  48 (char->integer #\0))
(test-eqv "char-(" 40 (char->integer #\())

; --- char eq comparisons ---
(test-true "alarm-eqv-7"     (eqv? #\alarm #\x7))
(test-true "backspace-eqv-8" (eqv? #\backspace #\x8))
(test-true "delete-eqv-127"  (eqv? #\delete #\x7F))
(test-true "escape-eqv-1B"   (eqv? #\escape #\x1B))
