# `(crab path)` — Pure path manipulation

CrabScheme stdlib module wrapping `std::path` (Rust). All paths are
Scheme strings; no filesystem access. Companion module
`(crab fs)` handles I/O.

## Procedures

```
(path-join str ...)            ;-> string
(path-basename str)            ;-> string  ; last component, or "" if none
(path-dirname str)             ;-> string  ; everything before the last component
(path-extension str)           ;-> string  ; extension without leading dot, or ""
(path-stem str)                ;-> string  ; basename minus extension
(path-is-absolute? str)        ;-> boolean
(path-with-extension str ext)  ;-> string  ; replace/add; "" strips
(path-components str)          ;-> list of strings
```

## Example

```scheme
(display (path-join "/etc" "nginx" "sites-enabled" "default.conf"))
;; /etc/nginx/sites-enabled/default.conf

(display (path-basename "/var/log/syslog.1"))    ;; syslog.1
(display (path-stem     "/var/log/syslog.1"))    ;; syslog
(display (path-extension "/var/log/syslog.1"))   ;; 1
(display (path-with-extension "report.txt" "md"));; report.md
(display (path-components "/usr/local/bin"))     ;; (/ usr local bin)
```

## Edge cases

| Input                              | basename | dirname | extension | stem |
|------------------------------------|----------|---------|-----------|------|
| `"file.txt"`                       | `file.txt` | `""`    | `txt`     | `file` |
| `"/abs/path/file"`                 | `file`     | `/abs/path` | `""`  | `file` |
| `"trailing/"`                      | `trailing` | `""`    | `""`      | `trailing` |
| `".hidden"`                        | `.hidden`  | `""`    | `""`      | `.hidden` |
| `""`                               | `""`       | `""`    | `""`      | `""` |

The empty-string cases mirror Rust's `Path` semantics: an empty path
has no components.
