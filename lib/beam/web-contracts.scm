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

;;; --- HTTP status codes --------------------------------------------
;;;
;;; Named constants for the codes most handlers actually use, plus
;;; range predicates and one-call response helpers so the common
;;; cases read declaratively:
;;;
;;;   (respond! h http/ok body)            => 200
;;;   (ok        h body)                   => 200
;;;   (created   h body)                   => 201
;;;   (no-content h)                       => 204
;;;   (bad-request   h "invalid id")       => 400
;;;   (unauthorized  h "no token")         => 401
;;;   (forbidden     h "denied")           => 403
;;;   (not-found     h)                    => 404
;;;   (unprocessable h "missing 'name'")   => 422
;;;   (server-error  h "boom")             => 500
;;;
;;; Range predicates:
;;;
;;;   (http-informational? n)  100..199
;;;   (http-success?       n)  200..299
;;;   (http-redirect?      n)  300..399
;;;   (http-client-error?  n)  400..499
;;;   (http-server-error?  n)  500..599
;;;   (http-error?         n)  client OR server

;; --- 2xx success
(define http/ok                  200)
(define http/created             201)
(define http/accepted            202)
(define http/no-content          204)
;; --- 3xx redirection
(define http/moved-permanently   301)
(define http/found               302)
(define http/see-other           303)
(define http/not-modified        304)
(define http/temporary-redirect  307)
(define http/permanent-redirect  308)
;; --- 4xx client errors
(define http/bad-request         400)
(define http/unauthorized        401)
(define http/forbidden           403)
(define http/not-found           404)
(define http/method-not-allowed  405)
(define http/conflict            409)
(define http/gone                410)
(define http/length-required     411)
(define http/payload-too-large   413)
(define http/unsupported-media   415)
(define http/teapot              418)
(define http/unprocessable       422)
(define http/too-many-requests   429)
;; --- 5xx server errors
(define http/internal-error      500)
(define http/not-implemented     501)
(define http/bad-gateway         502)
(define http/service-unavailable 503)
(define http/gateway-timeout     504)

(define (http-informational? n) (and (integer? n) (>= n 100) (<= n 199)))
(define (http-success?       n) (and (integer? n) (>= n 200) (<= n 299)))
(define (http-redirect?      n) (and (integer? n) (>= n 300) (<= n 399)))
(define (http-client-error?  n) (and (integer? n) (>= n 400) (<= n 499)))
(define (http-server-error?  n) (and (integer? n) (>= n 500) (<= n 599)))
(define (http-error?         n) (or (http-client-error? n) (http-server-error? n)))

;; One-call response helpers. The body argument is optional on
;; status codes that conventionally have no body (204, 304, 404);
;; default body is the empty string. Helpers always return unspec
;; just like web-respond!.
(define (ok            h body)        (web-respond! h http/ok body))
(define (created       h body)        (web-respond! h http/created body))
(define (accepted      h body)        (web-respond! h http/accepted body))
(define (no-content    h)             (web-respond! h http/no-content ""))
(define (bad-request   h msg)         (web-respond! h http/bad-request msg))
(define (unauthorized  h msg)         (web-respond! h http/unauthorized msg))
(define (forbidden     h msg)         (web-respond! h http/forbidden msg))
(define (not-found     h . rest)
  (web-respond! h http/not-found
                (if (null? rest) "Not Found" (car rest))))
(define (method-not-allowed h)        (web-respond! h http/method-not-allowed "Method Not Allowed"))
(define (conflict      h msg)         (web-respond! h http/conflict msg))
(define (unprocessable h msg)         (web-respond! h http/unprocessable msg))
(define (too-many-requests h msg)     (web-respond! h http/too-many-requests msg))
(define (server-error  h msg)         (web-respond! h http/internal-error msg))
(define (service-unavailable h msg)   (web-respond! h http/service-unavailable msg))

;;; --- Middleware ---------------------------------------------------
;;;
;;; The cs-web framework ships four built-in Layers (Trace,
;;; RequestId, Timeout, CatchPanic). These primops install them
;;; on a *Building* server (between create and start). First call
;;; ends up the OUTERMOST wrapper — so a request flows through
;;; them in the order installed, and a response back through them
;;; in reverse order.
;;;
;;;   (web-layer-trace!      sid)         ; stderr access log
;;;   (web-layer-request-id! sid)         ; x-request-id inject + echo
;;;   (web-layer-timeout!    sid 30000)   ; 504 after N ms
;;;   (web-layer-catch-panic! sid)        ; user-visible panic guard
;;;
;;; Note: cs-web's `serve` already wraps the whole router in
;;; an always-on CatchPanic, so a panicking handler can never
;;; crash the connection task even without `web-layer-catch-panic!`.
;;; Adding the explicit layer just gives you a more localized
;;; recovery scope (handler panic → 500 immediately, vs. let it
;;; propagate to the outer guard).

;;; --- Short aliases ------------------------------------------------
;;;
;;; The canonical primops are spelled `web-request-*` / `web-respond!`
;;; for discoverability — embedders grep the codebase and find them.
;;; Inside a Scheme handler the long names get tedious, so these
;;; aliases are exported alongside. Pick whichever you prefer; both
;;; resolve to the same procedure at the same call cost.

(define req-method  web-request-method)
(define req-path    web-request-path)
(define req-body    web-request-body)
(define req-param   web-request-param)
(define req-params  web-request-params)
(define req-header  web-request-header)
(define req-headers web-request-headers)
(define respond!    web-respond!)

;;; `with-request` — drop the explicit handle inside a lexical
;;; scope by binding local macros that capture it. Zero runtime
;;; cost (each `(param "k")` expands to a direct
;;; `(web-request-param h "k")` call at compile time) and the
;;; short names disappear cleanly outside the form, so nothing
;;; pollutes the surrounding namespace.
;;;
;;; Example:
;;;
;;;   (receive
;;;     [('*web-request* h)
;;;      (with-request h
;;;        (let ((id  (param  "id"))
;;;              (tok (header "x-token")))
;;;          (if (integer-string? id)
;;;              (respond! 200 (string-append "ok " id))
;;;              (respond! 400 "bad id"))))])
;;;
;;; The local names bound inside `body ...`:
;;;
;;;   (method)      => (web-request-method  h)
;;;   (path)        => (web-request-path    h)
;;;   (body)        => (web-request-body    h)
;;;   (param "k")   => (web-request-param   h "k")
;;;   (params)      => (web-request-params  h)
;;;   (header "k")  => (web-request-header  h "k")
;;;   (headers)     => (web-request-headers h)
;;;   (respond! s b) => (web-respond!       h s b)
;;;
;;; `body` is both a top-level alias and a local nullary form — the
;;; local one wins inside the macro's scope by hygienic shadowing.

(define-syntax with-request
  (syntax-rules ()
    [(_ h body* ...)
     (let-syntax ([method   (syntax-rules () [(_)    (web-request-method  h)])]
                  [path     (syntax-rules () [(_)    (web-request-path    h)])]
                  [body     (syntax-rules () [(_)    (web-request-body    h)])]
                  [param    (syntax-rules () [(_ k)  (web-request-param   h k)])]
                  [params   (syntax-rules () [(_)    (web-request-params  h)])]
                  [header   (syntax-rules () [(_ k)  (web-request-header  h k)])]
                  [headers  (syntax-rules () [(_)    (web-request-headers h)])]
                  [respond! (syntax-rules () [(_ s b*) (web-respond!      h s b*)])])
       body* ...)]))

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
