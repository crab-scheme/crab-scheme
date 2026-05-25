; R6RS++ §6 — continuation marks.
;
; As of issue #36 the continuation-mark forms are implemented
; NATIVELY and are tail-safe, so this library no longer defines
; them in Scheme:
;
;   * `with-continuation-mark` is a core special form recognised by
;     the expander (cs-expand). It installs `key -> val` on the
;     CURRENT continuation frame for the dynamic extent of its body.
;     Because the body is in tail position, a wcm reached through a
;     tail call replaces the frame's mark for that key rather than
;     accumulating — so a tail loop with a wcm runs in constant
;     mark-space (R7RS / Racket tail-mark semantics).
;
;   * `current-continuation-marks` is a builtin: zero args returns
;     the full `(key . val)` alist innermost-first; one arg returns
;     the list of values marked under that key, innermost-first.
;
; Both work identically on the tree-walker and bytecode-VM tiers
; (the walker keeps a depth-tagged mark stack on its EvalCtx; the VM
; keeps a per-frame mark slot). See
; docs/adr/0027-tail-safe-continuation-marks.md.
;
; This file is retained as a no-op so existing load/import sites keep
; working; it intentionally defines nothing.
;
; Superseded caveats from the naive (parameter-based) implementation:
;   1. NOT tail-safe  -> fixed (constant mark-space in tail loops).
;   2. Same-key marks in tail position now REPLACE (Racket semantics)
;      rather than accumulate.
;
; Still deferred (post-#36): a first-class `continuation-mark-set`
; value you can capture and pass around independent of the current
; dynamic context (`continuation-mark-set->list`, etc.).
