# `(crab json)` — JSON encode/decode

CrabScheme stdlib module wrapping `serde_json`. Iter 6 of the
stdlib-modules spec.

## Procedures

```
(json-parse str)         ;-> scheme value  ; errors on malformed input
(json-stringify val)     ;-> string        ; compact (no whitespace)
(json-pretty val)        ;-> string        ; 2-space indent
```

## Mapping

| JSON                | Scheme                                  |
|---------------------|-----------------------------------------|
| object              | alist (list of (key . value) pairs)     |
| array               | Scheme list                             |
| string              | string                                  |
| number (integer)    | fixnum (in i64 range; else flonum)      |
| number (fractional) | flonum                                  |
| `true` / `false`    | `#t` / `#f`                             |
| `null`              | `'()`                                   |

`null` and `[]` both decode to `'()`; a typed `json-null?` sentinel
will land with `Value::Opaque`. Object keys are alphabetically
ordered on decode (serde_json default Map behavior).

## Example

```scheme
(import (crab json))

(define cfg (json-parse (read-file-string "config.json")))
(display (cdr (assoc "port" cfg))) (newline)

(write-file-string "out.json"
  (json-pretty '(("name" . "alice")
                 ("scores" . (10 20 30)))))
```
