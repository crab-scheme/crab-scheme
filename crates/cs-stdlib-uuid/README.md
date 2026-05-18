# `(crab uuid)` — UUID v4 / v7 generation and parsing

CrabScheme stdlib module wrapping the `uuid` crate. Iter 5 of the
stdlib-modules spec.

UUIDs are passed around as 36-char hyphenated hex strings to avoid
the opaque-payload problem; a typed `uuid?` predicate + handle
lands when `Value::Opaque` does.

## Procedures

```
(uuid-v4)              ;-> string         ; random UUIDv4
(uuid-v7)              ;-> string         ; timestamp + random UUIDv7
(uuid-valid? str)      ;-> boolean        ; parseable as a UUID?
(uuid-version str)     ;-> fixnum or #f   ; UUID version; #f if unparseable
```

## Example

```scheme
(import (crab uuid))

(define id (uuid-v7))
(display "session id: ") (display id) (newline)
(display "version: ")    (display (uuid-version id)) (newline)

(if (uuid-valid? user-input)
    (display "ok\n")
    (display "invalid id\n"))
```

## v4 vs v7

- **v4** is fully random. Use when the only requirement is
  uniqueness and unpredictability.
- **v7** prefixes a millisecond Unix timestamp, so v7 UUIDs sort
  approximately by creation time. Better for database primary keys,
  log correlation IDs, and anything that benefits from
  insertion-time-sorted storage.
