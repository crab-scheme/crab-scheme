; Conformance test for `(crab xml)` — parse / navigate / build / serialize.

(test-section "(crab xml) — parse + accessors")
(define doc (xml-parse "<user id=\"1\" role=\"admin\"><name>Ada</name><email>ada@x.io</email></user>"))
(test-true "xml-parse returns an element" (xml-element? doc))
(test-false "a plain string is not an element" (xml-element? "user"))
(test-equal "xml-tag" "user" (xml-tag doc))
(test-equal "xml-attr (existing)" "1" (xml-attr doc "id"))
(test-equal "xml-attr (second)" "admin" (xml-attr doc "role"))
(test-false "xml-attr (missing) is #f" (xml-attr doc "missing"))
(test-equal "xml-attrs alist" '(("id" . "1") ("role" . "admin")) (xml-attrs doc))

(define kids (xml-children doc))
(test-equal "two child elements" 2 (length kids))
(test-equal "first child tag" "name" (xml-tag (car kids)))
(test-equal "child text" "Ada" (xml-text (car kids)))

(test-section "(crab xml) — text content")
(test-equal "xml-text concatenates descendant text" "Adaada@x.io" (xml-text doc))

(test-section "(crab xml) — build + serialize")
(define el (xml-make "p" '(("class" . "intro")) (list "hello " (xml-make "b" '() (list "world")))))
(test-true "xml-make builds an element" (xml-element? el))
(test-equal "serialize attrs + nested + text"
            "<p class=\"intro\">hello <b>world</b></p>"
            (xml->string el))
(test-equal "an empty element self-closes" "<br/>" (xml->string (xml-make "br" '() '())))

(test-section "(crab xml) — escaping + round-trip")
(test-equal "text is escaped"
            "<x>a &amp; b &lt; c</x>"
            (xml->string (xml-make "x" '() (list "a & b < c"))))
(test-equal "attribute values are escaped"
            "<x y=\"&quot;q&quot;\"/>"
            (xml->string (xml-make "x" (list (cons "y" "\"q\"")) '())))
(define rt (xml-parse (xml->string el)))
(test-equal "round-trip preserves the tag" "p" (xml-tag rt))
(test-equal "round-trip preserves an attribute" "intro" (xml-attr rt "class"))

(test-section "(crab xml) — errors")
(test-true "malformed XML raises"
           (guard (e (#t #t)) (xml-parse "<unclosed>") #f))
