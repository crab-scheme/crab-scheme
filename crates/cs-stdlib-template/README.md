# `(crab template)` — mustache-style text/HTML templating

CrabScheme stdlib module — the `(crab …)` answer to Go's `text/template`
+ `html/template`, Python's jinja, and Clojure's selmer. Pure Rust, no
dependencies, wasm-portable. Pairs with `(crab http)`.

## Syntax

The data context is an association list of `(name . value)` pairs;
values may be strings, numbers, booleans, nested alists, or lists.

- `{{ key }}` — interpolate, **HTML-escaped**. `key` may be a dotted
  path (`{{ user.name }}`) into nested alists, or `.` for the current
  item inside an `each`.
- `{{{ key }}}` — interpolate **raw** (unescaped).
- `{{#each items}} … {{/each}}` — repeat the body once per element of
  the list at `items`, with that element as the context.
- `{{#if key}} … {{/if}}` — render the body when `key` is truthy
  (present, not `#f`, not an empty string or list).

## Procedures

```
(template-render template data)  ;-> string  ; render template against the data alist
(html-escape string)             ;-> string  ; escape & < > " '
```

## Example

```scheme
(import (crab template))

(template-render
  "<h1>{{title}}</h1><ul>{{#each items}}<li>{{.}}</li>{{/each}}</ul>"
  '(("title" . "Crabs") ("items" . ("Ada" "Alan"))))
; => "<h1>Crabs</h1><ul><li>Ada</li><li>Alan</li></ul>"

(template-render
  "{{#if admin}}welcome, {{user.name}}{{/if}}"
  '(("admin" . #t) ("user" . (("name" . "root")))))
; => "welcome, root"
```

Interpolation escapes by default; reach for `{{{ }}}` only for values
you trust. An unclosed `{{#each}}`/`{{#if}}` raises an error.
