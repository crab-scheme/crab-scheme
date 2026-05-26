; R6RS++ §3 Phase 2C — record-definition shorthands.
;
; `define-record-type` is the underlying primitive; this library
; provides terser sugar for the common case of "all-immutable
; record with auto-generated accessors". The expansion delegates
; to define-record-type, which handles ctor / predicate / accessor
; name generation (NAME, make-NAME, NAME?, NAME-FIELD).
;
;   (define-record point (x y))
;     ==> (define-record-type point (fields x y))
;
;   (define-record-mutable counter (n))
;     ==> (define-record-type counter
;           (fields (mutable n counter-n set-counter-n!)))
;
; The mutable form is intentionally a separate macro rather than a
; keyword on `define-record`: syntax-rules can't synthesize the
; accessor/mutator names (`counter-n`, `set-counter-n!`) from the
; type and field names, so an expander-level shortcut is used
; instead. See expand_define_record_type for the (mutable FIELD)
; one-arg shorthand, which the expander recognizes and auto-names.

(define-syntax-parser define-record
  ((_ name:id (field ...))
   (define-record-type name (fields field ...))))

(define-syntax-parser define-record-mutable
  ((_ name:id (field ...))
   (define-record-type name (fields (mutable field) ...))))
