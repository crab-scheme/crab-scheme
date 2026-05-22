; The Scheme-on-actor bridge — real actors whose *logic is Scheme*.
;
;   crabscheme run lib/consensus/actor-bridge-demo.scm
;
; WHY THIS EXISTS (Constitution Article I — the code is Scheme):
; An actor body runs on a worker of the multi-thread runtime, so it must be
; `Send`. But a Scheme procedure is `Rc`-based and therefore `!Send` — a raw
; `(lambda …)` literally cannot cross onto the actor's thread. `spawn-source`
; is the bridge: you hand it the actor body as SOURCE plus the name of a
; top-level procedure; on the actor's own thread it builds a fresh runtime,
; loads the source (every Rc value stays thread-local), and runs that
; procedure. Messages and arguments cross as data, exactly like a mailbox send.
;
; This file proves the whole actor substrate working from pure Scheme for the
; first time: two actors, real OS threads, message passing with PID
; round-trip (send / self / raw-receive), and a process-global table as the
; shared channel the (non-actor) main thread observes.

(make-table 'bridge "set")        ; process-global, shared across every runtime

; ============================================================
; server: square whatever number it is asked for, reply to the asker. Loops
; forever; each request is `(reply-pid . n)`.
; ============================================================
(define server-src "
  (define (server)
    (let loop ()
      (let ((m (raw-receive)))
        (send (car m) (* (cdr m) (cdr m)))
        (loop))))")

; ============================================================
; client: ask the server for 8^2, then stash the reply where main can read it.
; It finds the server by reading the server's PID out of the shared table
; (a PID round-trips through Scheme as a printable symbol that `send` parses).
; ============================================================
(define client-src "
  (define (client)
    (let ((srv (table-lookup 'bridge \"server-pid\")))
      (send srv (cons (self) 8))
      (let ((answer (raw-receive)))
        (table-insert! 'bridge \"answer\" answer))))")

; ---- wire it up (main thread) ----
(define srv (spawn-source server-src 'server))
(table-insert! 'bridge "server-pid" srv)   ; publish the server's PID
(define cli (spawn-source client-src 'client))

; ---- main thread waits for the cross-thread result ----
; main is not an actor, so it busy-waits on the shared table. The actors run
; in parallel on separate worker threads, so this spin does not block them.
(define (await-answer)
  (let loop ((i 0))
    (let ((a (table-lookup 'bridge "answer")))
      (cond (a a)
            ((> i 5000000) (error "actor-bridge demo: timed out waiting for reply"))
            (else (loop (+ i 1)))))))

(define answer (await-answer))
(display "spawn-source two-actor bridge: 8^2 = ") (display answer) (newline)
(if (= answer 64)
    (begin (display "actor-bridge demo: all checks passed") (newline))
    (error "actor-bridge demo: wrong answer" answer))
