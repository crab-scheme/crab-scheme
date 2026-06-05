;;; (crab actor) — synchronous request/reply over actor mailboxes.
;;;
;;; A bundled Scheme library: global at startup in `actor`-enabled builds
;;; (`(import (crab actor))` is a no-op). Built on the actor primitives
;;; `send`, `raw-receive`, and `self`.
;;;
;;; This is the BEAM-style `(call pid msg)` the runtime always documented
;;; (see lib/beam/prelude.scm) but never shipped runnably — the prelude's
;;; version routes through a stub `match-and-bind` and a `(spawn thunk)`
;;; that the `!Send` Value type cannot support. Before this lib, every
;;; call site hand-rolled the request/reply by hand, e.g. crab-cache's
;;; `ask-local`:
;;;
;;;     (send target (cons (self) msg)) (raw-receive)
;;;
;;; Now it is just `(call target msg)`.
;;;
;;; Contract (the gen_server convention): the peer handles a message of
;;; shape `(cons sender payload)` and answers with `(send sender result)`.
;;; `call` assumes request/reply discipline — the next message delivered
;;; to the caller is its reply — exactly as the hand-rolled form did.

;; Synchronous request/reply. Sends `(sender . msg)` to `target`, then
;; blocks on `raw-receive` for the reply and returns it.
(define (call target msg)
  (send target (cons (self) msg))
  (raw-receive))

;; Server side. A request delivered by `call` is `(sender . payload)`:
(define (call-sender request) (car request))   ; the caller's pid
(define (call-message request) (cdr request))  ; the payload it sent

;; Answer a request by sending `answer` back to its sender; mirrors
;; `(send (call-sender request) answer)`.
(define (call-reply request answer)
  (send (call-sender request) answer))
