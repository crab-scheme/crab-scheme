# `(crab string)` — String operations beyond R6RS

CrabScheme stdlib module wrapping Rust `str` methods. R6RS already
covers `string-length`, `substring`, `string-append`, etc.; this
module adds the operations every script ends up reaching for.

## Procedures

```
(string-split str sep)              ;-> list of strings  ; empty sep splits each char
(string-join lst sep)               ;-> string
(string-trim str)                   ;-> string
(string-trim-left str)              ;-> string
(string-trim-right str)             ;-> string
(string-replace str old new)        ;-> string           ; all occurrences
(string-contains? str needle)       ;-> boolean
(string-starts-with? str prefix)    ;-> boolean
(string-ends-with? str suffix)      ;-> boolean
(string-pad-left  str width [ch])   ;-> string           ; ch defaults to space
(string-pad-right str width [ch])   ;-> string
(string-repeat str n)               ;-> string
```

## Example

```scheme
(define line "  foo,bar, baz ")
(display (string-split (string-trim line) ","))
;; (foo bar  baz)

(display (string-replace "hello world" "world" "stdlib"))
;; hello stdlib

(display (string-pad-left "42" 5 #\0))   ;; 00042
```
