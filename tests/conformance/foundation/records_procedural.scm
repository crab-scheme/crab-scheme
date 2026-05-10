(test-section "R6RS (rnrs records procedural) foundation")

;; --- bare RTD, predicate, accessor, mutator ---
(define ptrtd
  (make-record-type-descriptor 'pt #f #f #f #f
    (vector (list 'mutable 'x) (list 'mutable 'y))))
(define ptcd (make-record-constructor-descriptor ptrtd #f #f))
(define mkpt (record-constructor ptcd))
(define ppred (record-predicate ptrtd))
(define ptx (record-accessor ptrtd 0))
(define pty (record-accessor ptrtd 1))
(define ptx-set! (record-mutator ptrtd 0))

(test-true  "rtd?"                (record-type-descriptor? ptrtd))
(test-false "rtd-not-record"      (record? ptrtd))
(test-equal "rtd-name"            'pt (record-type-name ptrtd))
(test-equal "rtd-field-names"     '(x y)
            (vector->list (record-type-field-names ptrtd)))
(test-true  "rtd-field-0-mut"     (record-field-mutable? ptrtd 0))
(test-equal "rtd-no-parent"       #f (record-type-parent ptrtd))

(define a (mkpt 1 2))
(test-true  "a-record?"           (record? a))
(test-true  "a-pred"              (ppred a))
(test-false "pred-on-fixnum"      (ppred 7))
(test-equal "a-x"                 1 (ptx a))
(test-equal "a-y"                 2 (pty a))
(ptx-set! a 99)
(test-equal "a-x-after-mut"       99 (ptx a))
(test-true  "rtd-roundtrip"       (eq? ptrtd (record-rtd a)))

;; --- inheritance ---
(define cprtd
  (make-record-type-descriptor 'cpt ptrtd #f #f #f
    (vector (list 'mutable 'z))))
(define cpcd (make-record-constructor-descriptor cprtd #f #f))
(define mkcp (record-constructor cpcd))
(define cpred (record-predicate cprtd))
(define cpz (record-accessor cprtd 0))

(test-true  "child-rtd?"          (record-type-descriptor? cprtd))
(test-equal "child-parent"        ptrtd (record-type-parent cprtd))
(test-equal "child-only-own-1"    1 (vector-length (record-type-field-names cprtd)))
(test-true  "parent-not-child"    (not (cpred a)))

(define b (mkcp 10 20 30))   ; parent fields first, then own
(test-true  "child-record?"       (record? b))
(test-true  "child-is-parent"     (ppred b))    ; descendant predicate
(test-true  "child-is-child"      (cpred b))
(test-equal "child-parent-x"      10 (ptx b))
(test-equal "child-parent-y"      20 (pty b))
(test-equal "child-own-z"         30 (cpz b))

;; --- mismatched types ---
(test-true "accessor-rejects-wrong-type"
  (guard (c (#t #t))
    (cpz a)
    #f))

;; --- arity check on constructor ---
(test-true "ctor-arity-mismatch"
  (guard (c (#t #t))
    (mkpt 1 2 3)
    #f))

;; --- record-type-uid stays as #f when not provided ---
(test-equal "rtd-uid-default"     #f (record-type-uid ptrtd))
(test-equal "rtd-sealed-default"  #f (record-type-sealed? ptrtd))
(test-equal "rtd-opaque-default"  #f (record-type-opaque? ptrtd))
(test-true  "rtd-generative-default" (record-type-generative? ptrtd))

;; --- non-record gives sensible result ---
(test-false "fixnum-not-record"   (record? 42))
(test-false "string-not-record"   (record? "hello"))
(test-false "pair-not-record"     (record? '(a b c)))
