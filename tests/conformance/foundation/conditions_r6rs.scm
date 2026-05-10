(test-section "R6RS condition types: simple, compound, hierarchy")

; --- simple constructors + predicates ---
(define m (make-message-condition "boom"))
(test-true  "message-cond-cond?"     (condition? m))
(test-true  "message-cond-pred"      (message-condition? m))
(test-equal "message-cond-accessor"  "boom" (condition-message m))
(test-false "message-cond-not-error" (error? m))

(define i (make-irritants-condition '(1 2 3)))
(test-true  "irritants-cond-pred"    (irritants-condition? i))
(test-equal "irritants-cond-accessor" '(1 2 3) (condition-irritants i))

(define w (make-warning))
(test-true  "warning-pred"           (warning? w))
(test-false "warning-not-serious"    (serious-condition? w))

(define s (make-serious-condition))
(test-true  "serious-pred"           (serious-condition? s))
(test-false "serious-not-warning"    (warning? s))

(define e (make-error))
(test-true  "error-pred"             (error? e))
(test-true  "error-is-serious"       (serious-condition? e))
(test-false "error-not-violation"    (violation? e))

(define v (make-violation))
(test-true  "viol-pred"              (violation? v))
(test-true  "viol-is-serious"        (serious-condition? v))
(test-false "viol-not-error"         (error? v))

(define av (make-assertion-violation))
(test-true  "av-pred"                (assertion-violation? av))
(test-true  "av-is-violation"        (violation? av))
(test-true  "av-is-serious"          (serious-condition? av))

(define ncv (make-non-continuable-violation))
(test-true  "ncv-pred"               (non-continuable-violation? ncv))
(test-true  "ncv-is-violation"       (violation? ncv))

(define wh (make-who-condition 'caller))
(test-true  "who-pred"               (who-condition? wh))
(test-equal "who-accessor"           'caller (condition-who wh))

; --- compound conditions ---
(define c (condition (make-error) (make-message-condition "bad") (make-irritants-condition '(x y))))
(test-true  "compound-cond?"         (condition? c))
(test-true  "compound-error?"        (error? c))
(test-true  "compound-msg?"          (message-condition? c))
(test-true  "compound-irritants?"    (irritants-condition? c))
(test-equal "compound-message"       "bad" (condition-message c))
(test-equal "compound-irritants"     '(x y) (condition-irritants c))

; condition flattens nested compounds
(define c2 (condition c (make-who-condition 'sub)))
(test-true  "nested-cond?"           (condition? c2))
(test-true  "nested-error?"          (error? c2))
(test-equal "nested-who"             'sub (condition-who c2))
(test-equal "nested-message"         "bad" (condition-message c2))

; simple-conditions returns one element per simple
(test-eqv   "simple-conds-len-3"     3 (length (simple-conditions c)))
(test-eqv   "simple-conds-len-4"     4 (length (simple-conditions c2)))

; --- non-conditions ---
(test-false "cond-num"               (condition? 42))
(test-false "cond-list"              (condition? '(1 2 3)))
(test-false "cond-vector-untagged"   (condition? #(a b c)))
(test-false "cond-string"            (condition? "hi"))
(test-false "msg-on-num"             (message-condition? 42))
(test-false "error-on-num"           (error? 42))

; --- error builtin produces a proper compound ---
(define caught
  (with-exception-handler (lambda (c) c) (lambda () (error "bang" 1 2))))
(test-true  "raised-cond?"           (condition? caught))
(test-true  "raised-error?"          (error? caught))
(test-true  "raised-msg?"            (message-condition? caught))
(test-true  "raised-irritants?"      (irritants-condition? caught))
(test-equal "raised-message"         "bang" (condition-message caught))
(test-equal "raised-irritants"       '(1 2) (condition-irritants caught))
(test-true  "raised-error-object?"   (error-object? caught))
(test-equal "raised-eom"             "bang" (error-object-message caught))
(test-equal "raised-eoi"             '(1 2) (error-object-irritants caught))

; raised condition without irritants
(define caught2
  (with-exception-handler (lambda (c) c) (lambda () (error "no irritants"))))
(test-equal "no-irritants-msg"       "no irritants" (condition-message caught2))
(test-false "no-irritants-cond?"     (irritants-condition? caught2))

; bare simple (no compound wrapper applied externally) is still recognised
; — make-message-condition returns a one-element compound, and condition-*
; predicates/accessors work transparently.
(test-true  "bare-msg-cond?"         (condition? (make-message-condition "x")))
(test-true  "bare-msg-msg-cond?"     (message-condition? (make-message-condition "x")))

; ---- R6RS §7.2 — &i/o family ----------------------------------------
(test-section "R6RS condition types: &i/o + violation subtypes")

(define ioe (make-i/o-error))
(test-true  "io-error-pred"           (i/o-error? ioe))
(test-true  "io-error-error?"         (error? ioe))
(test-true  "io-error-serious?"       (serious-condition? ioe))
(test-false "io-error-not-violation"  (violation? ioe))

(define ior (make-i/o-read-error))
(test-true  "io-read-pred"            (i/o-read-error? ior))
(test-true  "io-read-is-i/o"          (i/o-error? ior))

(define iow (make-i/o-write-error))
(test-true  "io-write-pred"           (i/o-write-error? iow))
(test-true  "io-write-is-i/o"         (i/o-error? iow))

(define ipe (make-i/o-invalid-position-error 42))
(test-true  "ipe-pred"                (i/o-invalid-position-error? ipe))
(test-equal "ipe-position"            42 (i/o-error-position ipe))
(test-true  "ipe-is-i/o"              (i/o-error? ipe))

(define ifn (make-i/o-filename-error "/tmp/x"))
(test-true  "ifn-pred"                (i/o-filename-error? ifn))
(test-equal "ifn-filename"            "/tmp/x" (i/o-error-filename ifn))
(test-true  "ifn-is-i/o"              (i/o-error? ifn))

(define ifp (make-i/o-file-protection-error))
(test-true  "ifp-pred"                (i/o-file-protection-error? ifp))
(test-true  "ifp-is-filename"         (i/o-filename-error? ifp))

(define iro (make-i/o-file-is-read-only-error))
(test-true  "iro-pred"                (i/o-file-is-read-only-error? iro))
(test-true  "iro-is-protection"       (i/o-file-protection-error? iro))
(test-true  "iro-is-filename"         (i/o-filename-error? iro))
(test-true  "iro-is-i/o"              (i/o-error? iro))

(define iae (make-i/o-file-already-exists-error))
(test-true  "iae-pred"                (i/o-file-already-exists-error? iae))
(test-true  "iae-is-filename"         (i/o-filename-error? iae))

(define ine (make-i/o-file-does-not-exist-error))
(test-true  "ine-pred"                (i/o-file-does-not-exist-error? ine))
(test-true  "ine-is-filename"         (i/o-filename-error? ine))

(define ipo (make-i/o-port-error 'a-port))
(test-true  "ipo-pred"                (i/o-port-error? ipo))
(test-equal "ipo-port"                'a-port (i/o-error-port ipo))
(test-true  "ipo-is-i/o"              (i/o-error? ipo))

(define idec (make-i/o-decoding-error))
(test-true  "idec-pred"               (i/o-decoding-error? idec))
(test-true  "idec-is-port"            (i/o-port-error? idec))

(define ienc (make-i/o-encoding-error #\é))
(test-true  "ienc-pred"               (i/o-encoding-error? ienc))
(test-equal "ienc-char"               #\é (i/o-encoding-error-char ienc))
(test-true  "ienc-is-port"            (i/o-port-error? ienc))

; ---- violation subtypes ----
(define sv (make-syntax-violation '(foo bar) 'bar))
(test-true  "syntax-pred"             (syntax-violation? sv))
(test-true  "syntax-is-violation"     (violation? sv))
(test-true  "syntax-is-serious"       (serious-condition? sv))
(test-equal "syntax-form"             '(foo bar) (syntax-violation-form sv))
(test-equal "syntax-subform"          'bar (syntax-violation-subform sv))

(define uv (make-undefined-violation))
(test-true  "undefined-pred"          (undefined-violation? uv))
(test-true  "undefined-is-violation"  (violation? uv))

(define lv (make-lexical-violation))
(test-true  "lexical-pred"            (lexical-violation? lv))
(test-true  "lexical-is-violation"    (violation? lv))

(define ir (make-implementation-restriction-violation))
(test-true  "impl-pred"               (implementation-restriction-violation? ir))
(test-true  "impl-is-violation"       (violation? ir))

(define ni (make-no-infinities-violation))
(test-true  "ni-pred"                 (no-infinities-violation? ni))
(test-true  "ni-is-impl"              (implementation-restriction-violation? ni))
(test-true  "ni-is-violation"         (violation? ni))

(define nn (make-no-nans-violation))
(test-true  "nn-pred"                 (no-nans-violation? nn))
(test-true  "nn-is-impl"              (implementation-restriction-violation? nn))
