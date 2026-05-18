# `(crab process)` — Synchronous subprocess execution

CrabScheme stdlib module wrapping `std::process::Command` and the
`which` crate. Iter 3 of the stdlib-modules spec.

Async / spawn-and-keep-handle patterns need an opaque-payload Scheme
value the FFI layer doesn't yet expose; those land in a follow-up
iter once `Value::Opaque` ships. For now this module provides the
synchronous convenience — fire, wait, collect.

## Procedures

```
(run cmd argv)              ;-> (exit-code stdout-string stderr-string)
(run cmd argv stdin)        ;-> (exit-code stdout-string stderr-string)
(run/status cmd argv)       ;-> exit-code     ; inherits stdio; useful for build tools

(which cmd)                 ;-> string or #f  ; first PATH match
```

- `cmd` is the executable name (or absolute path).
- `argv` is a Scheme list of strings — every arg passed individually,
  no shell parsing. To use a shell, do `(run "sh" (list "-c" "foo | bar"))`.
- Optional third arg to `run` is a string written to the child's
  stdin before reading its stdout/stderr.

## Example

```scheme
(import (crab process))

(define result (run "git" (list "rev-parse" "HEAD")))
(define exit-code (car result))
(define commit (string-trim (car (cdr result))))

(if (= exit-code 0)
    (begin (display "head: ") (display commit) (newline))
    (begin (display "git failed: ") (display (car (cdr (cdr result)))) (newline)))

;; piping input
(define lower (run "tr" (list "a-z" "A-Z") "hello, world\n"))
(display (car (cdr lower)))     ; HELLO, WORLD

;; just check exit status
(if (= 0 (run/status "test" (list "-f" "/etc/hostname")))
    (display "etc/hostname exists\n")
    (display "missing\n"))
```

## Notes

- `run` captures stdout and stderr into Scheme strings using
  `String::from_utf8_lossy` — invalid UTF-8 bytes become U+FFFD.
- `run` reads the child's full output into memory before returning;
  streaming a large output through a port lands when `(crab fs)` /
  the future opaque-handle work make port-wrapped child stdio
  available.
- `which` returns `#f` when the lookup fails for *any* reason
  (not found, IO error, ambiguity). The error detail is currently
  discarded; future iter may add `which/explain`.
