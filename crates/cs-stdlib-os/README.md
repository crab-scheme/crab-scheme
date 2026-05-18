# `(crab os)` — Environment, identity, platform

CrabScheme stdlib module wrapping `std::env`, `std::process`, and a
small `gethostname` dependency. Iter 3 of the stdlib-modules spec.

## Procedures

```
;; environment
(get-env name)              ;-> string or #f
(set-env! name value)       ;-> unspecified
(unset-env! name)           ;-> unspecified
(env-vars)                  ;-> list of (k . v) pairs

;; working directory
(current-directory)         ;-> string
(change-directory path)     ;-> unspecified

;; process identity
(process-id)                ;-> fixnum
(parent-process-id)         ;-> fixnum  (0 on platforms without a stable accessor)
(hostname)                  ;-> string
(username)                  ;-> string or #f  (USER / LOGNAME / USERNAME)

;; platform metadata
(platform)                  ;-> string  ("linux", "macos", "windows", ...)
(architecture)              ;-> string  ("x86_64", "aarch64", ...)

;; exit
(exit)                      ;-> never returns; exit code 0
(exit code)                 ;-> never returns; exit code `code`
```

## Example

```scheme
(import (crab os))

(display "running on ") (display (platform))
(display "/") (display (architecture)) (newline)

(display "user: ")  (display (or (username) "anonymous")) (newline)
(display "cwd:  ")  (display (current-directory))         (newline)

(set-env! "MY_TOOL_DEBUG" "1")
(display "debug = ") (display (get-env "MY_TOOL_DEBUG")) (newline)
```

## Notes

- `set-env!` / `unset-env!` wrap `std::env::set_var` / `remove_var`,
  which became `unsafe` in Rust 1.86 because they can race with reads
  from other threads. CrabScheme is single-threaded, so the safety
  invariant holds within the runtime; the caller is responsible if
  there are other OS threads in the process (e.g. embedded
  CrabScheme inside a multi-threaded host).
- `env-vars` skips entries whose name or value isn't valid UTF-8 —
  `std::env::vars()` already does this internally.
- `parent-process-id` is `0` on platforms where `std` doesn't expose
  a stable accessor (Windows). On Unix it reads
  `std::os::unix::process::parent_id()`.
- `platform` and `architecture` are returned as strings rather than
  symbols until the FFI layer gains a SymbolTable-aware symbol
  constructor.
