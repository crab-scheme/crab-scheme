# `(crab tty)` — Terminal detection + size

CrabScheme stdlib module. Iter 13 of the stdlib-modules spec.

Small surface for the "is my output going to a terminal" and
"how wide is it" queries that every CLI tool reaches for. Cursor
movement, color control, raw-mode toggling, and interactive line
editing need a richer crate (`crossterm` / `console`) and design
discussion; deferred.

## Procedures

```
(tty-stdin?)    ;-> boolean
(tty-stdout?)   ;-> boolean
(tty-stderr?)   ;-> boolean
(terminal-size) ;-> (cols rows) or #f when no tty / unknown
```

## Example

```scheme
(import (crab tty))

(if (tty-stdout?)
    (display "\x1b;[1mhello\x1b;[0m\n")    ; ANSI bold when on a terminal
    (display "hello\n"))                    ; plain when piped to a file

(let ((s (terminal-size)))
  (if s
      (begin (display "terminal: ") (display (car s)) (display "×") (display (cadr s)) (newline))
      (display "no terminal attached\n")))
```
