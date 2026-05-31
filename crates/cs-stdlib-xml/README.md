# `(crab xml)` — XML parsing and serialization

CrabScheme stdlib module — the `(crab …)` answer to Python's `xml`,
Go's `encoding/xml`, and Clojure's `data.xml`. Parsing uses
[`roxmltree`]; serialization is hand-written. Pure Rust → ships in
`wasm-stdlib`.

## Element representation

An element is an opaque vector `#('__xml__ tag attrs children)`:

- `tag` — the element name (string)
- `attrs` — an alist of `(name . value)` string pairs
- `children` — a list of nested elements and/or text nodes (strings)

(A string-keyed tree rather than symbol-based SXML, because the FFI
layer can't intern symbols. Build trees with `xml-make`; read them
with the accessors.)

## Procedures

```
(xml-parse string)                  ;-> element     ; raises on malformed XML
(xml-element? value)                ;-> boolean
(xml-tag element)                   ;-> string
(xml-attrs element)                 ;-> alist of (name . value)
(xml-attr element name)             ;-> string | #f
(xml-children element)              ;-> list of elements + text strings
(xml-text element)                  ;-> string      ; all descendant text
(xml-make tag attrs children)       ;-> element
(xml->string element)               ;-> string      ; escaped XML
```

## Example

```scheme
(import (crab xml))

(define doc (xml-parse "<user id=\"1\"><name>Ada</name></user>"))
(xml-tag doc)                        ; => "user"
(xml-attr doc "id")                  ; => "1"
(xml-text (car (xml-children doc)))  ; => "Ada"

(xml->string
  (xml-make "p" '(("class" . "intro")) (list "hello " (xml-make "b" '() (list "world")))))
; => "<p class=\"intro\">hello <b>world</b></p>"
```

## Notes

- Parsing drops whitespace-only text between elements (typical for data
  XML); text with content is kept as a string child.
- Comments and processing instructions are skipped.
- Serialization escapes `& < >` in text and `& < "` in attribute values,
  and self-closes childless elements (`<br/>`).

[`roxmltree`]: https://github.com/RazrFalcon/roxmltree
