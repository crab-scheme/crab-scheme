# `(crab format)` — Printf-style formatting

CrabScheme stdlib module providing Common Lisp-style `~`-directive
string formatting. Iter 4 of the stdlib-modules spec.

## Procedures

```
(format-string fmt args...)  ;-> string
```

## Directives

| Directive | Behavior |
|---|---|
| `~a` | Display the next arg (humane — strings unquoted). |
| `~s` | Write the next arg (readable — strings quoted with `\` escapes). |
| `~d` | Decimal integer. |
| `~x` | Hex integer (lowercase). |
| `~X` | Hex integer (uppercase). |
| `~%` | Newline. |
| `~~` | Literal `~`. |

Mismatch between directive count and arg count raises. Unknown
directives raise.

## Example

```scheme
(display (format-string "~a is ~d years old~%" "alice" 30))
;; alice is 30 years old

(display (format-string "0x~X bytes" 4096))
;; 0x1000 bytes

(display (format-string "set ~s to ~s" 'name "alice"))
;; set name to "alice"
```

## Not yet supported

- Padding / width directives (`~5d`, `~,2f`, `~10@a`)
- Float-specific directives (`~f`, `~e`, `~g`)
- Conditional `~[…~]`, iteration `~{…~}`, case conversion `~(…~)`

These extend the surface naturally when needed — the parser already
handles the directive lookup; adding a new arm in the `render` match
is the entire work.
