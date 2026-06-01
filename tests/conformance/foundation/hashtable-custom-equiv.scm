; Regression: hashtable-update! on a custom-equiv hashtable (one built
; with make-hashtable + a user equivalence procedure, e.g. equal?).
; Previously it routed through `ht_eq`, which panics on the Custom kind;
; it now applies the user's equiv proc like set!/ref/contains!/delete!.

(test-section "hashtable-update! on a custom-equiv table")

(define h (make-hashtable equal-hash equal?))

; Absent key: the default seeds the update proc.
(hashtable-update! h '(1 2) (lambda (c) (+ c 1)) 0)
(test-equal "update! creates from the default" 1 (hashtable-ref h '(1 2) #f))

; Present key: the proc receives the current value.
(hashtable-update! h '(1 2) (lambda (c) (+ c 10)) 0)
(test-equal "update! transforms the existing value" 11 (hashtable-ref h '(1 2) #f))

; equal? semantics: a freshly-built, structurally-equal key matches.
(test-equal "custom equiv matches equal? keys" 11 (hashtable-ref h (list 1 2) #f))

(test-section "other custom-equiv ops still work")
(test-true "contains? finds the key" (hashtable-contains? h '(1 2)))
(hashtable-delete! h '(1 2))
(test-false "delete! removes it" (hashtable-contains? h '(1 2)))
