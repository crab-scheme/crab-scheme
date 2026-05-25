; (lang passthrough-reader) — a minimal R6RS++ `#!lang` library
; that demonstrates the parse-time custom reader protocol shipped
; in issue #10.
;
; The exported `reader` procedure is called by the host with the
; remaining source text (everything after the `#!lang` line) as a
; Scheme string. It returns a list of datums that the expander
; consumes in place of the host reader's output.
;
; This implementation defers entirely to the host reader by
; opening the body as a string input port and calling `read` in a
; loop until end-of-file. The resulting file therefore behaves
; identically to one with no `#!lang` header — useful as a
; sanity-check and as a starting point for languages that want to
; read host syntax first and then post-process the datums.
;
; ## Authoring a non-trivial reader
;
; Replace the loop body to perform per-datum rewriting (e.g.
; macro-like substitution before expansion), prepend / append
; auxiliary forms, or even tokenize a non-Scheme surface syntax.
; The contract is just `string -> list-of-datums`; anything
; achievable by a Scheme procedure is fair game.
;
; ## Scope and limitations (iter 1)
;
; - Datum spans synthesized by a custom reader collapse to the
;   first byte of the body file. Diagnostic ranges within
;   reader-produced forms are therefore coarse but unambiguous.
; - The optional `expander` and `base-env` exports from the spec
;   are not yet honored — only `reader` is wired through. A lang
;   that exports them is loaded normally; those bindings just
;   sit unused.

(library (lang passthrough-reader)
  (export reader)
  (import (rnrs))

  (define (reader body-str)
    (let ((port (open-input-string body-str)))
      (let loop ((acc '()))
        (let ((datum (read port)))
          (if (eof-object? datum)
              (reverse acc)
              (loop (cons datum acc))))))))
