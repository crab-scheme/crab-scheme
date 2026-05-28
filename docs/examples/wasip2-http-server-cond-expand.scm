;; #9 iter-6 — choosing an HTTP-server shape per target via
;; `(crab-target)`. The Scheme code below is a single source that
;; works on both native (accept-loop shape via tiny_http) and on
;; `wasm32-wasip2` (handler-callback shape via
;; `wasi:http/incoming-handler`). See ADR 0033 for the shape
;; divergence + tradeoffs.

(import (crab http))

(define (handle-request method url headers body)
  ;; A pure-Scheme handler: returns (values status headers body).
  ;; Same shape on both targets.
  (values 200
          '(("content-type" . "text/plain"))
          #vu8(72 101 108 108 111 33)))   ; "Hello!"

(cond
  ;; wasip2 — handler callback. Register the lambda; the runtime
  ;; calls it once per incoming request.
  ((string=? (crab-target) "wasi-p2")
   (http-incoming-handler
    (lambda (req)
      ;; Pull req fields if needed (a future iter-5b wires this);
      ;; for now drive the pure handler with empty placeholders.
      (handle-request 'GET "" '() #vu8()))))

  ;; native — accept loop on a bound socket.
  ((string=? (crab-target) "native")
   (let ((srv (http-server-bind "127.0.0.1:8080")))
     (let loop ()
       (let ((req (http-server-accept srv)))
         (when req
           (call-with-values
             (lambda () (handle-request
                          (http-request-method req)
                          (http-request-url req)
                          (http-request-headers req)
                          (http-request-body req)))
             (lambda (status headers body)
               (http-respond req status headers body)))))
       (loop))))

  ;; wasi-p1 (or any other) — no usable HTTP-server world; bail out
  ;; with a clear error rather than picking the wrong shape silently.
  (else
   (error 'main
          "(crab http) server has no shape for this target"
          (crab-target))))
