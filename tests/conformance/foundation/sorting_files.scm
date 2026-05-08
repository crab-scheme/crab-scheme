(test-section "Sorting + file I/O + port-eof?")

; list-sort
(test-equal "list-sort-asc"
  '(1 1 2 3 4 5 5 6 9)
  (list-sort < '(3 1 4 1 5 9 2 6 5)))
(test-equal "list-sort-desc"
  '(9 5 4 3 2 1)
  (list-sort > '(3 1 4 5 9 2)))
(test-equal "list-sort-empty" '() (list-sort < '()))
(test-equal "list-sort-single" '(42) (list-sort < '(42)))
(test-equal "list-sort-strings"
  '("a" "b" "c")
  (list-sort string<? '("c" "a" "b")))

; vector-sort
(test-equal "vector-sort"
  #(1 2 3 5 8 9)
  (vector-sort < #(5 2 8 1 9 3)))

; vector-sort! mutates
(define vs (vector 5 2 8 1 9 3))
(vector-sort! < vs)
(test-equal "vector-sort!-mutates"
  #(1 2 3 5 8 9)
  vs)

; sort with custom comparator (sort by car of pairs)
(test-equal "sort-pairs-by-key"
  '((1 . a) (2 . b) (3 . c))
  (list-sort (lambda (x y) (< (car x) (car y)))
             '((3 . c) (1 . a) (2 . b))))

; sort stability isn't required by R6RS but our insertion-sort is stable
(test-equal "sort-equal-keys-preserved"
  '((1 . first) (1 . second) (2 . third))
  (list-sort (lambda (x y) (< (car x) (car y)))
             '((1 . first) (1 . second) (2 . third))))

; file-exists? for known/missing
(test-true  "file-exists-/etc/hosts" (file-exists? "/etc/hosts"))
(test-false "file-missing"           (file-exists? "/no/such/file/abcdef"))

; open-input-file + read content
(define hp (open-input-file "/etc/hosts"))
(test-true "open-input-file-port?" (port? hp))
(test-true "open-input-file-input-port?" (input-port? hp))

; port-eof? on fresh port should be #f (not at end yet for non-empty file)
; or #t for empty input
(define empty-p (open-string-input-port ""))
(test-true "port-eof?-on-empty" (port-eof? empty-p))

(define non-empty-p (open-string-input-port "data"))
(test-false "port-eof?-on-fresh" (port-eof? non-empty-p))
(read-char non-empty-p)
(read-char non-empty-p)
(read-char non-empty-p)
(read-char non-empty-p)
(test-true "port-eof?-after-read-all" (port-eof? non-empty-p))
