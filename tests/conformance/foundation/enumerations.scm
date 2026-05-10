; M9 FR-1 — `(rnrs enums)` (R6RS §13).
;
; Iter 1 lands this test against the not-yet-implemented enumeration
; builtins. Iter 2 adds the implementation; this file is the
; executable spec.
;
; Until iter 2 lands, every test below errors as "undefined variable:
; make-enumeration" and the file's pass count is 0. The vm_conformance
; harness in `crates/cs-runtime/tests/` reports the file but doesn't
; fail the build if it skips a missing builtin (the prelude's test
; helpers catch the error and tally it as a fail).

(test-section "enumerations — basic predicates")

; Universe construction.
(define colors (make-enumeration '(red green blue)))

(test-true "enum-set? on a make-enumeration result"
  (enum-set? colors))

(test-false "enum-set? on a non-enum-set"
  (enum-set? 'red))

; Universe equality and listing.
(test-equal "enum-set->list returns the universe symbols in order"
  '(red green blue)
  (enum-set->list colors))

(test-equal "enum-set-universe of an enum-set is itself"
  (enum-set->list colors)
  (enum-set->list (enum-set-universe colors)))

(test-section "enumerations — indexer + constructor")

; Indexer: maps each symbol to its 0-based position.
(define color-index (enum-set-indexer colors))

(test-eqv "indexer returns 0 for the first symbol" 0 (color-index 'red))
(test-eqv "indexer returns 1 for the second symbol" 1 (color-index 'green))
(test-eqv "indexer returns 2 for the third symbol" 2 (color-index 'blue))
(test-equal "indexer returns #f for a non-member"
  #f
  (color-index 'purple))

; Constructor: builds an enum-set subset from a list of symbols.
(define color-cons (enum-set-constructor colors))

(test-equal "constructor with a single member produces a 1-element set"
  '(green)
  (enum-set->list (color-cons '(green))))

(test-equal "constructor with all members reproduces the universe"
  '(red green blue)
  (enum-set->list (color-cons '(red green blue))))

(test-section "enumerations — set operations")

(define rg (color-cons '(red green)))
(define gb (color-cons '(green blue)))
(define empty-colors (color-cons '()))
(define full-colors (color-cons '(red green blue)))

; Membership.
(test-true "enum-set-member? finds a present symbol"
  (enum-set-member? 'red rg))
(test-false "enum-set-member? rejects an absent symbol"
  (enum-set-member? 'blue rg))

; Subset, equality.
(test-true "subset? on equal sets" (enum-set-subset? rg rg))
(test-true "subset? on a strict subset"
  (enum-set-subset? (color-cons '(red)) rg))
(test-false "subset? on a non-subset"
  (enum-set-subset? gb rg))

(test-true "enum-set=? on equal sets" (enum-set=? rg (color-cons '(red green))))
(test-false "enum-set=? on different sets" (enum-set=? rg gb))

; Union / intersection / difference / complement.
(test-equal "union: rg ∪ gb = {red, green, blue}"
  '(red green blue)
  (enum-set->list (enum-set-union rg gb)))

(test-equal "intersection: rg ∩ gb = {green}"
  '(green)
  (enum-set->list (enum-set-intersection rg gb)))

(test-equal "difference: rg \\ gb = {red}"
  '(red)
  (enum-set->list (enum-set-difference rg gb)))

(test-equal "complement of empty = full universe"
  '(red green blue)
  (enum-set->list (enum-set-complement empty-colors)))

(test-equal "complement of full = empty"
  '()
  (enum-set->list (enum-set-complement full-colors)))
