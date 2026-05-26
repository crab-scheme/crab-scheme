;;; lib/beam/web-server.scm
;;;
;;; Declarative server building + middleware composition on top of
;;; the cs-web primops. Three pieces:
;;;
;;;   - `define-server`          declares a server, its layers,
;;;                              access log, and routes in one form
;;;   - `define-handler`         declares a request handler with a
;;;                              middleware chain that short-circuits
;;;                              on the first middleware that responds
;;;   - `web-forward!`           re-sends a web-request to another
;;;                              actor (Scheme-level middleware
;;;                              chains)
;;;
;;; Plus a handful of pre-built middleware procedures:
;;;
;;;   `require-header`            400 if absent
;;;   `require-auth-token`        401 if not equal to expected
;;;   `enforce-content-type`      415 if mismatch
;;;
;;; The combination subsumes most of what an `axum`-style Rust
;;; app expresses as `.layer()` chains, with the difference that
;;; Scheme middleware runs INSIDE the actor (so it has full access
;;; to !Send Scheme runtime state — bindings, hash-tables, etc.).
;;; Rust-side Layers (Trace, RequestId, Timeout, CatchPanic) still
;;; wrap the whole stack; install via `(middleware ...)` in
;;; `define-server` or `(web-layer-* sid)` directly.

;;; ----- web-forward! ----------------------------------------------
;;;
;;; Hand off a request to another actor. The downstream actor
;;; receives `('*web-request* h)` with the SAME handle, so it can
;;; read every field via `web-request-*` and respond when ready.
;;; The slab entry stays valid until `web-respond!` consumes it,
;;; so both actors COULD respond; first one wins. In practice the
;;; forwarding actor should not also respond — return from its
;;; receive clause without calling `web-respond!`.

(define (web-forward! h pid)
  (send pid (list '*web-request* h)))

;;; ----- run-middleware-chain --------------------------------------
;;;
;;; A middleware procedure has signature `(h) -> 'continue | _`.
;;;
;;;   `'continue`     pass through to the next middleware / final
;;;                   handler
;;;   anything else   middleware already responded via `web-respond!`
;;;                   or `(ok h ...)` etc.; chain stops
;;;
;;; `run-middleware-chain` walks the list left-to-right, invoking
;;; each. The first middleware that returns non-`'continue` ends
;;; the chain. If every middleware passes, the final handler runs.

(define (run-middleware-chain h mws final)
  (cond
    [(null? mws) (final h)]
    [else
     (let ([rv ((car mws) h)])
       (if (eq? rv 'continue)
           (run-middleware-chain h (cdr mws) final)
           rv))]))

;;; ----- define-handler --------------------------------------------
;;;
;;; Declares a procedure that runs a middleware chain in front of a
;;; main body. Used inside an actor's receive clause:
;;;
;;;   (define-handler users-handler
;;;     (middleware auth-check rate-limit)
;;;     (lambda (h)
;;;       (with-request h
;;;         (let ([id (param "id")])
;;;           (ok h (string-append "user " id))))))
;;;
;;;   (receive
;;;     [('*web-request* h) (users-handler h) (loop)])
;;;
;;; The macro is shape-driven on a `(middleware ...)` literal so
;;; the chain reads inline with the lambda — same pattern as
;;; `define-behavior`'s clause syntax.

(define-syntax-parser define-handler
  #:literals (middleware)
  [(_ name (middleware mw ...) body-lambda)
   (define (name h)
     (run-middleware-chain h (list mw ...) body-lambda))]
  [(_ name body-lambda)
   (define (name h)
     (body-lambda h))])

;;; ----- define-server ---------------------------------------------
;;;
;;; Declarative top-level server. Builds a server slot, installs
;;; layers, registers routes, and binds the slot ID to `name`.
;;; `(server-start! name)` separately kicks off the accept loop —
;;; users that want to add routes dynamically can still mutate the
;;; building slot between `define-server` and `server-start!`.
;;;
;;; Action clauses (syntax-rules literals: `middleware`,
;;; `access-log`, `route`, `static`, `timeout`, `request-id`,
;;; `trace`, `catch-panic`):
;;;
;;;   (middleware request-id trace (timeout 5000))
;;;       Each item is a built-in Rust Layer to install in order.
;;;       Symbols are atomic layers; `(timeout N)` is a layer with
;;;       arg.
;;;
;;;   (access-log "table-name")
;;;       Install the access-log Layer writing to the named
;;;       cs-table OrderedSet.
;;;
;;;   (route 'GET "/path" (static "body"))
;;;   (route 'GET "/path" (static "body" 418))
;;;       Register a static route. Status defaults to 200.
;;;
;;;   (route 'POST "/path" pid)
;;;   (route 'POST "/path" pid 5000)
;;;       Register an actor-backed route; the actor must already
;;;       be spawned. Timeout defaults to 30 s.
;;;
;;; Example:
;;;
;;;   (define-server my-app "127.0.0.1:8080"
;;;     (middleware request-id trace (timeout 30000))
;;;     (access-log "http-access")
;;;     (route 'GET  "/health"   (static "ok"))
;;;     (route 'GET  "/version"  (static "cs-web 0.0.1"))
;;;     (route 'GET  "/users"    users-pid)
;;;     (route 'POST "/users"    users-pid))
;;;   (server-start! my-app)

(define-syntax-parser define-server
  [(_ name addr action ...)
   (define name
     (let ([__sid (web-server-create addr)])
       (server-action __sid action) ...
       __sid))])

(define-syntax-parser server-action
  #:literals (middleware access-log route)
  ;; Middleware list — expand each item via server-mw.
  [(_ sid (middleware m ...))
   (begin (server-mw sid m) ...)]
  ;; Access log → cs-table OrderedSet by name.
  [(_ sid (access-log table-name))
   (web-access-log! sid table-name)]
  ;; Static-body routes (with + without explicit status).
  [(_ sid (route method path (static body)))
   (web-route-static! sid method path body)]
  [(_ sid (route method path (static body status)))
   (web-route-static! sid method path body status)]
  ;; Actor-backed routes (with + without timeout-ms).
  [(_ sid (route method path pid))
   (web-route-actor! sid method path pid)]
  [(_ sid (route method path pid timeout-ms))
   (web-route-actor! sid method path pid timeout-ms)])

(define-syntax-parser server-mw
  #:literals (request-id trace catch-panic timeout layer-actor)
  [(_ sid request-id)            (web-layer-request-id! sid)]
  [(_ sid trace)                 (web-layer-trace! sid)]
  [(_ sid catch-panic)           (web-layer-catch-panic! sid)]
  [(_ sid (timeout ms))          (web-layer-timeout! sid ms)]
  ;; (layer-actor pid)            uses the default 30s decision
  ;; (layer-actor pid ms)         caps decisions at ms milliseconds
  [(_ sid (layer-actor pid))     (web-layer-actor! sid pid)]
  [(_ sid (layer-actor pid ms))  (web-layer-actor! sid pid ms)])

;;; Tidy aliases over the canonical primops — matches the
;;; `req-*` style used elsewhere in this library.

(define server-start! web-server-start)
(define server-stop!  web-server-stop)

;;; ----- Scheme actor as a Rust Layer ------------------------------
;;;
;;; A layer actor receives `('*web-request* h)` BEFORE the route's
;;; handler runs. It picks one of two outcomes per request:
;;;
;;;   (web-respond! h status body)   ; short-circuit — handler never runs
;;;   (web-continue! h)              ; pass through to the inner service
;;;
;;; Failing to call either within the timeout returns 504 to the
;;; client and the inner service is not called.
;;;
;;; A layer actor is "real" middleware in the Tower sense — it
;;; wraps the whole sub-service, can short-circuit independent of
;;; the actor handling the route, and composes with the built-in
;;; Rust layers (request-id, trace, timeout, catch-panic). Use
;;; via the (layer-actor pid [timeout-ms]) clause of
;;; define-server.
;;;
;;; Example:
;;;
;;;   (define (auth-layer)
;;;     (let loop ()
;;;       (receive
;;;         [('*web-request* h)
;;;          (if (eq? (web-request-header h "x-token") "sekret")
;;;              (web-continue! h)              ; pass through
;;;              (web-respond! h 401 "no"))])   ; short-circuit
;;;       (loop)))
;;;
;;;   (define auth-pid (spawn 'auth-layer))
;;;   (define-server my-app "127.0.0.1:8080"
;;;     (middleware request-id (layer-actor auth-pid 2000))
;;;     (route 'GET "/secure" handler-pid))
;;;
;;; The layer actor runs `web-continue!` for valid tokens; the
;;; framework then dispatches to handler-pid via the next layer.
;;; For invalid tokens, web-respond! short-circuits and the
;;; handler actor is never contacted.

;;; ----- Pre-built middleware --------------------------------------
;;;
;;; Each middleware proc has signature `(h) -> 'continue | _`.
;;; Failing middleware emits a response and returns anything other
;;; than `'continue` (the symbol `'responded` is the convention).

;;; `(require-header name [status [msg]])` — 400 if header absent.
;;; Returns a middleware procedure suitable for `define-handler`.
(define (require-header name . opts)
  (let ([status (if (pair? opts) (car opts) 400)]
        [msg    (if (and (pair? opts) (pair? (cdr opts)))
                    (cadr opts)
                    (string-append "missing header: " name))])
    (lambda (h)
      (if (web-request-header h name)
          'continue
          (begin (web-respond! h status msg) 'responded)))))

;;; `(require-auth-token expected [header-name])` — 401 if the
;;; named header (default `"x-token"`) isn't exactly `expected`.
(define (require-auth-token expected . opts)
  (let ([header-name (if (pair? opts) (car opts) "x-token")])
    (lambda (h)
      (let ([got (web-request-header h header-name)])
        (if (and (string? got) (string=? got expected))
            'continue
            (begin (web-respond! h 401 "unauthorized") 'responded))))))

;;; `(enforce-content-type expected)` — 415 if the request's
;;; content-type header isn't an exact match. Useful in front of
;;; JSON / form handlers.
(define (enforce-content-type expected)
  (lambda (h)
    (let ([got (web-request-header h "content-type")])
      (if (and (string? got) (string=? got expected))
          'continue
          (begin (web-respond! h 415 "unsupported media type") 'responded)))))
