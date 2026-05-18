# `(crab csv)` — CSV read/write

CrabScheme stdlib module wrapping the `csv` Rust crate. Iter 6 of
the stdlib-modules spec.

```
(csv-parse str)          ;-> list of (list of strings)
(csv-write rows)         ;-> string   ; each row is a list of strings
```

Default delimiters: `,` separator, `"` quote, `\\n` row terminator.
The header row (if any) is returned alongside the data rows — pull
the first element off the list when you want headers separate.

## Example

```scheme
(import (crab csv))
(import (crab fs))

(define rows (csv-parse (read-file-string "users.csv")))
(define headers (car rows))
(define data (cdr rows))
(display (length data)) (display " users") (newline)

(write-file-string "out.csv"
  (csv-write '(("name" "age") ("alice" "30") ("bob" "25"))))
```
