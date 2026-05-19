;;; lib/beam/web-contracts.scm
;;;
;;; Contract-driven validation for cs-web request handlers.
;;;
;;; The `check-request` macro composes with the existing contract
;;; combinators from `lib/contract/contract.scm` (`or/c`, `and/c`,
;;; `list/c`, `list-of/c`, `vector-of/c`, etc.). Each clause binds
;;; a name to a value extracted from the request and tests it
;;; against a contract; on the first failure the request short-
;;; circuits with a 400 response naming the failed field, on
;;; success the body of the macro runs with all the bindings in
;;; scope.
;;;
;;; Example:
;;;
;;;   (receive
;;;     [('*web-request* h)
;;;      (check-request h
;;;        [[id-str   (web-request-param  h "id")    integer-string?]
;;;         [auth     (web-request-header h "x-token") non-empty-string?]
;;;         [body     (web-request-body h)             json-string?]]
;;;        (web-respond! h 200 (build-payload id-str auth body)))])
;;;
;;; Three helpers come bundled because they show up in almost
;;; every web contract spec:
;;;
;;;   non-empty-string?   string?, len > 0
;;;   integer-string?     parses as an integer
;;;   json-string?        starts with `{`/`[`/`"`/digit/`t`/`f`/`n`
;;;                       (cheap structural sniff — real validation
;;;                       is the parser's job)

(define (non-empty-string? v)
  (and (string? v) (> (string-length v) 0)))

(define (integer-string? v)
  (and (string? v)
       (let ([n (string->number v)])
         (and n (integer? n)))))

(define (json-string? v)
  (and (string? v)
       (> (string-length v) 0)
       (let ([c (string-ref v 0)])
         (or (char=? c #\{)
             (char=? c #\[)
             (char=? c #\")
             (char=? c #\t)   ; true
             (char=? c #\f)   ; false
             (char=? c #\n)   ; null
             (char=? c #\-)
             (and (char>=? c #\0) (char<=? c #\9))))))

;;; `check-request` — declarative request validation.
;;;
;;; Each clause: `[name value-expr contract]`
;;;   - name        bound to `value-expr` in subsequent clauses
;;;                 and the success body.
;;;   - value-expr  evaluated in the order written; usually one
;;;                 of `(web-request-param h "k")`,
;;;                 `(web-request-header h "x")`,
;;;                 `(web-request-body h)`, etc.
;;;   - contract    any predicate / contract combinator; called
;;;                 against the bound `name`.
;;;
;;; If a contract returns #f, the macro emits a 400 with body
;;; `"invalid <name>"` and DOES NOT evaluate further clauses or
;;; the success body. The macro is non-cooperative — once 400
;;; is sent the request is finalized; the handler's outer
;;; `receive` loop continues normally.
(define-syntax check-request
  (syntax-rules ()
    [(_ h () success ...)
     (begin success ...)]
    [(_ h ([name val contract] rest ...) success ...)
     (let ([name val])
       (if (contract name)
           (check-request h (rest ...) success ...)
           (web-respond! h 400
                         (string-append "invalid " (symbol->string 'name)))))]))

;;; `with-validated-request` — sugar over `check-request` for the
;;; common case where every clause has the same shape: a
;;; required field extracted by one of the inspector primops,
;;; tested against a contract. Lets callers write:
;;;
;;;   (with-validated-request h
;;;     #:param  ([id        integer-string?])
;;;     #:header ([x-token   non-empty-string?])
;;;     #:body                json-string?
;;;     (lambda (id x-token body)
;;;       (web-respond! h 200 ...)))
;;;
;;; The macro fans out into `check-request` clauses for each
;;; declared field. Field names become parameter names in the
;;; success lambda.
;;;
;;; Implementation note: syntax-rules can't pattern-match
;;; keywords directly, so we use position-by-keyword binding via
;;; nested expansion. Both `#:param` and `#:header` clauses are
;;; required (use empty `()` if you don't need any). `#:body` is
;;; optional and binds a single `body` name.

(define-syntax with-validated-request
  (syntax-rules (#:param #:header #:body)
    ;; Form WITH body contract.
    [(_ h
        #:param  ([p-name p-contract] ...)
        #:header ([h-name h-contract] ...)
        #:body   b-contract
        success-lambda)
     (check-request h
       ([p-name (web-request-param  h (symbol->string 'p-name))  p-contract] ...
        [h-name (web-request-header h (symbol->string 'h-name))  h-contract] ...
        [body   (web-request-body   h)                           b-contract])
       (success-lambda p-name ... h-name ... body))]
    ;; Form WITHOUT body contract.
    [(_ h
        #:param  ([p-name p-contract] ...)
        #:header ([h-name h-contract] ...)
        success-lambda)
     (check-request h
       ([p-name (web-request-param  h (symbol->string 'p-name))  p-contract] ...
        [h-name (web-request-header h (symbol->string 'h-name))  h-contract] ...)
       (success-lambda p-name ... h-name ...))]))
