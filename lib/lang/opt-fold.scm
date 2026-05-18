; (lang opt-fold) — a minimal demonstration #!lang that installs
; the `constant-fold` optimizer pass at library-load time. Files
; declaring `#!lang opt-fold` thereby opt into constant folding
; for the rest of the file.
;
; This is the smallest concrete demonstration of ADR 0014's
; "#!lang library installs a pass" story. Production languages
; will install richer policies (e.g., 'monomorphize for typed,
; 'specialize for numeric DSLs).
;
; ## Scoping caveat (iter 5 MVP)
;
; The pass installation persists across file boundaries within
; the same session — there's no automatic cleanup when the
; importing file's evaluation finishes. Code that wants strict
; file-scope semantics should pair `#!lang opt-fold` with an
; explicit `(remove-optimizer-pass! 'constant-fold)` at end of
; file, or migrate to the post-iter-5 Phase-2E-parameter-backed
; active-passes (which gives parameterize scoping for free).

(library (lang opt-fold)
  (export)
  (import (rnrs))
  ; Installation is the library's load-time effect. Re-imports
  ; are no-ops because install-optimizer-pass! is idempotent.
  (install-optimizer-pass! 'constant-fold))
