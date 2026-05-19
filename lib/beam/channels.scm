;;; lib/beam/channels.scm — patterns + macros on top of the
;;; cs-channel primops.
;;;
;;; The primops live in the top-level env under the `channel`
;;; feature: make-channel, channel-send!, channel-try-send!,
;;; channel-recv, channel-try-recv, channel-close!, channel-closed?,
;;; channel-len, channel-capacity, channel?, channel-select.
;;;
;;; This library adds:
;;;
;;;   (with-channel (name expr) body ...)   ; auto-close on exit
;;;   (select clauses ...)                  ; first-ready dispatch
;;;   (channel-for-each proc ch)            ; drain until closed
;;;   (channel-drain-to-list ch)            ; consume to a list

;;; ----- with-channel ----------------------------------------------
;;;
;;; Binds `name` to the result of `expr` (typically a
;;; `(make-channel …)` call), runs body, then closes the channel
;;; — even if body raises. Returns body's last value.

(define-syntax with-channel
  (syntax-rules ()
    [(_ (name expr) body0 body ...)
     (let ([name expr])
       (let ([__result (begin body0 body ...)])
         (channel-close! name)
         __result))]))

;;; ----- select ----------------------------------------------------
;;;
;;; Wait on first-ready of several channel operations. Clause
;;; shapes:
;;;
;;;   [(recv ch)        var body ...]   ; body sees `var` bound
;;;                                     ; to the received value or
;;;                                     ; '*closed* sentinel
;;;   [(send! ch val)        body ...]  ; body runs after send
;;;   [(after ms)            body ...]  ; body runs if no other
;;;                                     ; clause ready in `ms`
;;;   [else                  body ...]  ; body runs if every other
;;;                                     ; clause would block (no
;;;                                     ; await happens)
;;;
;;; Fairness: by default, random pick among simultaneously-ready
;;; clauses (matches Go semantics). Use `(select-biased …)` for
;;; deterministic clause-order priority.
;;;
;;; Example:
;;;
;;;   (select
;;;     [(recv ch1) v     (display "got from ch1: ") (display v)]
;;;     [(recv ch2) v     (display "got from ch2: ") (display v)]
;;;     [(send! ch3 val)  (display "queued on ch3")]
;;;     [(after 1000)     (display "1s timeout")])

(define-syntax select
  (syntax-rules ()
    [(_ clause ...)
     (select-build #f () () clause ...)]))

(define-syntax select-biased
  (syntax-rules ()
    [(_ clause ...)
     (select-build #t () () clause ...)]))

;;; Helper: accumulate clause specs + thunks, then dispatch.
;;; Each thunk takes one arg (the received value for recv clauses,
;;; ignored otherwise) so the index-driven dispatch is uniform.

(define-syntax select-build
  (syntax-rules (recv send! after else)
    ;; No more clauses — emit the call + dispatch.
    ;; Use a single `let` (not `let*`) so all three references
    ;; to `__r` share one expander rename / hygienic mark.
    [(_ biased (spec ...) (thunk ...))
     (let ([__r (channel-select (list spec ...) biased)])
       ((list-ref (list thunk ...) (car __r)) (cdr __r)))]
    ;; (recv ch) var body...
    [(_ biased (spec ...) (thunk ...)
        [(recv ch) var body0 body ...]
        rest ...)
     (select-build biased
                   (spec ... (list 'recv ch))
                   (thunk ... (lambda (var) body0 body ...))
                   rest ...)]
    ;; (send! ch v) body...
    [(_ biased (spec ...) (thunk ...)
        [(send! ch v) body0 body ...]
        rest ...)
     (select-build biased
                   (spec ... (list 'send! ch v))
                   (thunk ... (lambda (__sel-ignored) body0 body ...))
                   rest ...)]
    ;; (after ms) body...
    [(_ biased (spec ...) (thunk ...)
        [(after ms) body0 body ...]
        rest ...)
     (select-build biased
                   (spec ... (list 'after ms))
                   (thunk ... (lambda (__sel-ignored) body0 body ...))
                   rest ...)]
    ;; else body...
    [(_ biased (spec ...) (thunk ...)
        [else body0 body ...]
        rest ...)
     (select-build biased
                   (spec ... (list 'else))
                   (thunk ... (lambda (__sel-ignored) body0 body ...))
                   rest ...)]))

;;; ----- channel-for-each -----------------------------------------
;;;
;;; Pull values from `ch` until it's closed-and-drained, calling
;;; `proc` on each. The *closed* sentinel itself is NOT passed to
;;; proc; the loop just terminates when it appears. Useful inside
;;; a worker actor's body.

(define (channel-for-each proc ch)
  (let loop ()
    (let ([v (channel-recv ch)])
      (cond
        [(eq? v '*closed*) (if #f #f)]   ; unspec
        [else (proc v) (loop)]))))

;;; ----- channel-drain-to-list -----------------------------------
;;;
;;; Block until the channel is closed; return all received values
;;; in send order. Useful in tests + small scripts; bad for hot
;;; production code (unbounded memory).

(define (channel-drain-to-list ch)
  (let loop ([acc '()])
    (let ([v (channel-recv ch)])
      (cond
        [(eq? v '*closed*) (reverse acc)]
        [else (loop (cons v acc))]))))
