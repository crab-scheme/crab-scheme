(test-section "R6RS import-spec modifiers: only / except / prefix / rename")

; --- bare library reference (no-op, just shape check) ---
(import (rnrs base))
(test-eqv "bare-import works" 5 (+ 2 3))

; --- only: accepted but not enforced; the listed names remain accessible ---
(import (only (rnrs base) car cdr cons))
(test-equal "only-listed-still-works" 1 (car '(1 2 3)))
; non-listed names still work because we don't have library scopes yet.
(test-equal "only-also-non-listed-works" '(2 3) (cdr '(1 2 3)))

; --- except: accepted, similarly non-restrictive ---
(import (except (rnrs base) length))
(test-eqv "except-non-listed-works" 5 (+ 2 3))
(test-eqv "except-listed-still-works" 3 (length '(a b c)))

; --- prefix: accepted; doesn't actually rename without a library manifest ---
(import (prefix (rnrs base) base:))
; Just verifying the form parses without error; the prefix doesn't
; create base:car etc. yet.
(test-eqv "prefix-form-accepted" 1 (car '(1)))

; --- rename: actually creates the renamed bindings via define synthesis ---
(import (rename (rnrs base) (car my-car) (cdr my-cdr)))
(test-equal "renamed-car"  1     (my-car '(1 2 3)))
(test-equal "renamed-cdr"  '(2 3) (my-cdr '(1 2 3)))
; Original names still resolvable.
(test-equal "rename-keeps-original" 1 (car '(1)))

; --- rename combined with multiple pairs ---
(import (rename (rnrs base) (length len) (reverse rev)))
(test-eqv "renamed-len"   3 (len '(a b c)))
(test-equal "renamed-rev" '(3 2 1) (rev '(1 2 3)))

; --- nested modifiers ---
(import (rename (only (rnrs base) +) (+ plus)))
(test-eqv "nested-rename" 7 (plus 3 4))

; --- malformed import-spec raises a syntax error at expand time ---
; We can't easily test expand-time errors from inside the runtime
; without specialized harness support, so the parsing tests above
; standing in for the structural validation.
