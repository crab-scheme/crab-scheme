; Conformance test for `(crab dict)` — association-list helpers.

(define d '((a . 1) (b . 2) (c . 3)))

(test-section "(crab dict) — basics")
(test-equal "dict-ref existing key" 2 (dict-ref d 'b))
(test-false "dict-ref missing key is #f" (dict-ref d 'z))
(test-equal "dict-ref missing key with default" 'none (dict-ref d 'z 'none))
(test-true "dict-has? existing" (dict-has? d 'a))
(test-false "dict-has? missing" (dict-has? d 'z))
(test-equal "dict-set replaces in place" '((a . 1) (b . 9) (c . 3)) (dict-set d 'b 9))
(test-equal "dict-set appends a new key" '((a . 1) (b . 2) (c . 3) (d . 4)) (dict-set d 'd 4))
(test-equal "dissoc removes a key" '((a . 1) (c . 3)) (dissoc d 'b))
(test-equal "dict-keys" '(a b c) (dict-keys d))
(test-equal "dict-vals" '(1 2 3) (dict-vals d))
(test-equal "zipmap pairs up keys and values" '((a . 1) (b . 2)) (zipmap '(a b) '(1 2)))
(test-equal "select-keys keeps a subset" '((a . 1) (c . 3)) (select-keys d '(a c)))

(test-section "(crab dict) — nested access")
(define nested '((user . ((name . "ada") (roles . ((admin . #t)))))))
(test-equal "get-in one level" "ada" (get-in nested '(user name)))
(test-equal "get-in deep" #t (get-in nested '(user roles admin)))
(test-false "get-in missing is #f" (get-in nested '(user age)))
(test-equal "get-in missing with default" 0 (get-in nested '(user age) 0))
(test-equal "assoc-in updates a nested value"
            "bob" (get-in (assoc-in nested '(user name) "bob") '(user name)))
(test-equal "assoc-in creates intermediate dicts"
            5 (get-in (assoc-in '() '(x y z) 5) '(x y z)))
(test-equal "update-in applies a function"
            2 (get-in (update-in '((n . 1)) '(n) (lambda (v) (+ v 1))) '(n)))

(test-section "(crab dict) — merge")
(test-equal "merge: later dict wins" 9 (dict-ref (merge '((a . 1)) '((a . 9) (b . 2))) 'a))
(test-equal "merge: combines keys" 2 (dict-ref (merge '((a . 1)) '((b . 2))) 'b))
