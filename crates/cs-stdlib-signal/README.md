# `(crab signal)` — Unix signal polling

CrabScheme stdlib module wrapping `signal-hook`. Iter 13 of the
stdlib-modules spec.

Signal handlers can't directly invoke Scheme thunks — signal
contexts forbid most allocation and CrabScheme's runtime is
single-threaded. This module installs a tiny reader thread per
watched signal that pushes into a shared queue; user code drains
the queue via `signal-poll`. Standard pattern: arm at startup,
poll in your event loop.

Windows builds (where `signal-hook` isn't available) register
stubs that always return `#f` from `signal-poll` and accept any
`signal-watch!` call as a no-op, so portable programs don't need
`cond-expand`.

## Procedures

```
(signal-watch! name)  ;-> unspec       ; arm the OS handler for `name`
(signal-poll)         ;-> string or #f ; next pending signal or #f if none
```

Supported signal names: `"SIGINT"`, `"SIGTERM"`, `"SIGHUP"`,
`"SIGQUIT"`, `"SIGUSR1"`, `"SIGUSR2"`. Anything else raises.

## Example

```scheme
(import (crab signal))

(signal-watch! "SIGINT")
(signal-watch! "SIGTERM")

(let loop ()
  (do-some-work)
  (let ((s (signal-poll)))
    (cond
      ((equal? s "SIGINT")  (display "interrupted, cleaning up\n") (cleanup))
      ((equal? s "SIGTERM") (display "termination requested\n") (cleanup))
      (else                 (loop)))))
```
