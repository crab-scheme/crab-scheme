# `(crab log)` — Leveled stderr logging

CrabScheme stdlib module. Iter 8 of the stdlib-modules spec.
Deliberately small — no global `tracing` subscriber to install,
no per-call allocation beyond what `format!` does. Each call
writes one line to stderr: `<UNIX_EPOCH_MS> <LEVEL> <MSG>`.

## Procedures

```
(log-trace msg…)
(log-debug msg…)
(log-info  msg…)
(log-warn  msg…)
(log-error msg…)

(log-set-level! level)        ;-> unspec   ; level: "off" / "error" / "warn" / "info" / "debug" / "trace"
(log-current-level)           ;-> string
```

Threshold defaults to `"info"`. `log-trace` only emits when level
is `"trace"`; `log-error` always emits unless level is `"off"`.

## Example

```scheme
(import (crab log))

(log-set-level! "debug")
(log-info  "starting server on port" 8080)
(log-debug "config loaded" 'with-flags '(verbose))
(log-error "fatal: " "out of memory")
```

## Output shape

```
1779094000000 INFO starting server on port 8080
1779094000001 DEBUG config loaded with-flags (verbose)
1779094000002 ERROR fatal:  out of memory
```

Structured-fields (JSON-formatted) output and pluggable sinks
(file rotation, syslog, network) land in a follow-up iter
alongside `Value::Opaque`.
