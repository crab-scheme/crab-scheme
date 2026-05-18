# `(crab regex)` — Regular expressions

CrabScheme stdlib module wrapping the `regex` Rust crate. Iter 4
of the stdlib-modules spec.

Patterns are passed in as strings on every call. A per-process LRU
cache (64 entries) memoizes compiled patterns so tight match loops
don't pay the compile cost on every iteration. When the FFI layer
gains an opaque-payload Scheme value, a typed `(compile-regex …)`
returning a reusable handle will replace this scheme.

## Procedures

```
(regex-match? pat str)        ;-> boolean        ; any match anywhere
(regex-find pat str)          ;-> string or #f   ; first match's matched text
(regex-find-all pat str)      ;-> list of strings
(regex-replace pat str repl)  ;-> string         ; first match only
(regex-replace-all pat str repl) ;-> string
(regex-split pat str)         ;-> list of strings ; split at each match
```

`pat` uses the Rust `regex` crate's syntax (Perl-compatible-ish; no
backrefs, no lookaround — see <https://docs.rs/regex/latest/regex/#syntax>).
Invalid patterns raise.

## Example

```scheme
(define ip "192.168.1.42")

(display (regex-match? "^\\d+\\.\\d+\\.\\d+\\.\\d+$" ip))  ;; #t

(display (regex-find-all "[0-9]+" "build 42 in 3 ms over 1024 entries"))
;; (42 3 1024)

(display (regex-replace-all "\\s+" "  too    many   spaces  " " "))
;; " too many spaces "

(display (regex-split "[,;]" "a,b;c,d"))
;; (a b c d)
```
