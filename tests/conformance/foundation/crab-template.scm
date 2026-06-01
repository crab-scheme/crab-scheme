; Conformance test for `(crab template)` — mustache-style rendering.

(test-section "(crab template) — interpolation")
(test-equal "simple interpolation" "Hello, Ada!"
            (template-render "Hello, {{name}}!" '(("name" . "Ada"))))
(test-equal "escaped by default" "&lt;b&gt;"
            (template-render "{{x}}" '(("x" . "<b>"))))
(test-equal "triple-brace is raw" "<b>"
            (template-render "{{{x}}}" '(("x" . "<b>"))))
(test-equal "dotted path into nested alist" "30"
            (template-render "{{user.age}}" '(("user" . (("age" . 30))))))
(test-equal "missing key renders empty" "[]"
            (template-render "[{{nope}}]" '()))

(test-section "(crab template) — each")
(test-equal "each over scalars" "<li>a</li><li>b</li>"
            (template-render "{{#each xs}}<li>{{.}}</li>{{/each}}" '(("xs" . ("a" "b")))))
(test-equal "each over alists, fields by name" "Ada:30,Bob:25,"
            (template-render "{{#each people}}{{name}}:{{age}},{{/each}}"
                             '(("people" . ((("name" . "Ada") ("age" . 30))
                                            (("name" . "Bob") ("age" . 25)))))))
(test-equal "each over empty list renders nothing" "X"
            (template-render "X{{#each xs}}Y{{/each}}" '(("xs" . ()))))

(test-section "(crab template) — if")
(test-equal "if true renders the body" "yes"
            (template-render "{{#if flag}}yes{{/if}}" '(("flag" . #t))))
(test-equal "if false renders nothing" ""
            (template-render "{{#if flag}}yes{{/if}}" '(("flag" . #f))))
(test-equal "if missing renders nothing" ""
            (template-render "{{#if flag}}yes{{/if}}" '()))
(test-equal "a non-empty list is truthy" "has"
            (template-render "{{#if xs}}has{{/if}}" '(("xs" . (1)))))

(test-section "(crab template) — nesting + html-escape")
(test-equal "each nested inside if" "<ul><li>1</li><li>2</li></ul>"
            (template-render
             "{{#if show}}<ul>{{#each xs}}<li>{{.}}</li>{{/each}}</ul>{{/if}}"
             '(("show" . #t) ("xs" . (1 2)))))
(test-equal "html-escape procedure" "a &amp; b &lt;x&gt;" (html-escape "a & b <x>"))

(test-section "(crab template) — errors")
(test-true "an unclosed block raises"
           (guard (e (#t #t)) (template-render "{{#each xs}}x" '(("xs" . (1)))) #f))
