# `(crab cli)` — command-line argument & flag parsing

CrabScheme stdlib module — the `(crab …)` answer to Python's
`argparse`, Go's `flag`, and Clojure's `tools.cli`. Pure Rust, no
external dependencies, portable to every target (including wasm32).

Parsing is a **pure function** of an option spec plus an argument
list — no global state, no I/O. You supply the argument list,
typically `(cdr (command-line))` (R6RS §6.4; the `car` is the
program name) or any list of strings.

## Procedures

```
(cli-option long short kind default help)  ;-> option   ; build one descriptor
(cli-option? value)                        ;-> boolean  ; descriptor predicate
(cli-parse options args)                   ;-> alist    ; parse args
(cli-usage prog description options)       ;-> string   ; formatted --help text
```

### Building options

`(cli-option long short kind default help)`:

- `long`    — string, the `--long` name (no leading dashes).
- `short`   — single-character string (matched as `-x`) or `#f`.
- `kind`    — one of `"flag"`, `"string"`, `"int"`, `"float"`.
              A `"flag"` takes no value and parses to a boolean;
              the others take a value.
- `default` — value used when the option is absent (usually `#f`
              for a flag).
- `help`    — help string or `#f`.

### Parse result

`(cli-parse options args)` returns an association list mapping each
option's `long` name to its parsed value (defaults filled in for
absent options), plus the key `"--"` whose value is the list of
positional arguments (everything that isn't an option, including
all tokens after a literal `--`). Look values up with `assoc`.

## Example

```scheme
(import (crab cli))

(define opts
  (list (cli-option "verbose" "v" "flag"   #f      "be loud")
        (cli-option "name"    #f  "string" "world" "who to greet")
        (cli-option "count"   "n" "int"    1       "repeat count")))

(define r (cli-parse opts (cdr (command-line))))

(when (cdr (assoc "verbose" r))
  (display "(verbose mode)\n"))

(let loop ((i (cdr (assoc "count" r))))
  (when (> i 0)
    (display "hello, ") (display (cdr (assoc "name" r))) (newline)
    (loop (- i 1))))

; remaining positional args:
(for-each (lambda (p) (display p) (newline))
          (cdr (assoc "--" r)))
```

To print help, format the spec with `cli-usage`:

```scheme
(display (cli-usage "greet" "Greet someone a number of times." opts))
```

## Accepted syntax

- `--long`, `--long=value`, `--long value`
- `-x`, `-x=value`, `-x value`
- a literal `--` ends option parsing; the rest are positional
- a lone `-` is a positional (stdin convention)

A value option consumes the following token as its value, so a value
that itself starts with `-` must use the `=` form (`--name=-5`); a
negative number passed as the next token still works (`--count -5`).
Short-flag clustering (`-abc`) is **not** supported in this version —
write `-a -b -c`. Unknown options, a missing value, or a value that
fails to parse as `int`/`float` raise an error (catch with `guard`
and show `cli-usage`).
