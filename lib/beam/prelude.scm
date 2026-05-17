; BEAM-style Scheme prelude for CrabScheme.
;
; Per the Rust/Scheme split in docs/research/beam_runtime_spec.md,
; everything in this file is the POLICY layer on top of four
; primops that cs-runtime exposes from cs-actor:
;
;   (spawn thunk)            ; -> ActorPid
;   (send pid value)         ; cast, fire-and-forget
;   (raw-receive [timeout])  ; -> Message (User|Exit|Down) or #f
;   (self)                   ; -> ActorPid of calling actor
;
; Plus cs-table primops:
;
;   (make-table name type)
;   (table-insert! name key value)
;   (table-lookup name key)
;   (table-delete! name key)
;   (table-fold name acc f)
;
; Plus cs-hotreload primops:
;
;   (load-module! name exports)         ; -> epoch
;   (lookup-code module fn-name)        ; -> proc or #f
;   (code-soft-purge! module holders)
;   (code-purge! module)
;   (code-versions module)              ; -> (old . current)
;
; This file is unloaded as a library / prelude before any user
; code runs. Status: design draft — the primops aren't yet wired
; from cs-runtime to the cs-actor/cs-table/cs-hotreload crates,
; so the code here is design-validating, not runnable.

; ============================================================
; PART 1 — selective receive with pattern matching (B-prelude)
; ============================================================
;
; (receive
;   ((pattern1) action1)
;   ((pattern2) action2)
;   ...
;   (after timeout-ms timeout-action))
;
; Tries each pattern against incoming messages in mailbox order.
; First match wins. If `(after TIMEOUT ACTION)` is present, the
; receive returns ACTION's value after TIMEOUT ms; otherwise it
; blocks forever.
;
; Implementation: a simple loop over raw-receive that pattern-
; matches and re-queues non-matching messages. For B-prelude
; this is a thin macro over `cond`; real selective-receive (with
; mailbox-order preservation across non-matches) lives in a
; later iter once mailbox introspection lands.

(define-syntax receive
  (syntax-rules (after)
    ((_ (pat action) ...)
     (let loop ()
       (let ((msg (raw-receive #f)))
         (cond
           ((match-and-bind msg pat action) ...)
           (else (loop))))))
    ((_ (pat action) ... (after timeout-ms timeout-action))
     (let ((deadline (+ (current-jiffy)
                        (* timeout-ms (/ (jiffies-per-second) 1000)))))
       (let loop ()
         (let* ((remaining (- deadline (current-jiffy)))
                (remaining-ms (if (< remaining 0)
                                  0
                                  (quotient (* remaining 1000)
                                            (jiffies-per-second))))
                (msg (raw-receive remaining-ms)))
           (cond
             ((not msg) timeout-action)
             ((match-and-bind msg pat action) ...)
             (else (loop)))))))))

; Stub macro: in a real impl this is a pattern compiler. For
; the prelude draft we expand to a simple equal? check.
(define-syntax match-and-bind
  (syntax-rules (_)
    ((_ msg _ action) action)
    ((_ msg pat action)
     (if (equal? msg pat) action #f))))

; ============================================================
; PART 2 — call (synchronous RPC) over send + receive
; ============================================================
;
; (call pid msg [timeout-ms]) sends msg to pid wrapped with our
; PID; pid replies by sending back to (self). We block on the
; receive until the reply arrives or timeout fires.
;
; The peer's contract: handle messages of shape
; (cons sender payload) and reply with (send sender result).
; That's the gen_server convention.

(define call
  (case-lambda
    ((pid msg)
     (call pid msg #f))
    ((pid msg timeout-ms)
     (let ((my-pid (self)))
       (send pid (cons my-pid msg))
       (if timeout-ms
           (receive
             ((reply) reply)
             (after timeout-ms
                    (error 'call "call timeout"
                           pid timeout-ms)))
           (receive
             ((reply) reply)))))))

; ============================================================
; PART 3 — link / monitor over spawn + system messages
; ============================================================
;
; Erlang's link/monitor primitives are built on the "system
; message" convention: when an actor exits, its system sends
; a Message::Exit to every linked actor and Message::Down to
; every monitor. Our cs-actor crate carries these as enum
; variants; the Scheme primop layer surfaces them as tagged
; lists:
;
;   '(*exit* <from-pid> <reason>)
;   '(*down* <ref-id> <pid> <reason>)
;
; (link pid) and (monitor pid) are primitives that arrange for
; those messages to be delivered. The default action on receiving
; an *exit* is to die yourself; (trap-exit! #t) converts the
; signal into an ordinary message you can pattern-match.

(define (link pid)
  (system-link! pid))

(define (unlink pid)
  (system-unlink! pid))

(define (monitor pid)
  (system-monitor! pid))

(define (demonitor ref-id)
  (system-demonitor! ref-id))

(define (trap-exit! enabled?)
  (system-trap-exit! enabled?))

; ============================================================
; PART 4 — supervisors (formerly cs-supervisor, now Scheme)
; ============================================================
;
; OTP's supervisor.erl is ~600 lines of Erlang on top of
; gen_server. Ours is ~300 lines of Scheme on top of
; spawn/receive/monitor.
;
; A supervisor is an actor that:
;   1. Spawns each child (according to its child-spec).
;   2. Monitors each child.
;   3. On a child exit, applies the configured strategy
;      (one-for-one / one-for-all / rest-for-one) to decide
;      which siblings to restart.
;   4. Tracks restart intensity ({max-restarts, period-seconds})
;      and exits itself if children crash too fast (escalating
;      to its own supervisor).
;
; Public API:
;   (make-supervisor name children
;     #:strategy 'one-for-one
;     #:intensity 1
;     #:period 5)
;     -> supervisor-pid
;
;   (supervisor-which-children sup-pid) -> alist (id . pid)
;   (supervisor-terminate-child sup-pid id)
;   (supervisor-restart-child sup-pid id)

(define-record-type <child-spec>
  (make-child-spec id start-thunk restart shutdown child-type)
  child-spec?
  (id child-spec-id)
  (start-thunk child-spec-start-thunk)
  (restart child-spec-restart)   ; 'permanent | 'transient | 'temporary
  (shutdown child-spec-shutdown) ; 'brutal-kill | <ms> | 'infinity
  (child-type child-spec-type))  ; 'worker | 'supervisor

(define (make-supervisor name children
                         #:strategy [strategy 'one-for-one]
                         #:intensity [intensity 1]
                         #:period [period 5])
  (spawn
    (lambda ()
      (trap-exit! #t)
      (supervisor-loop name children strategy intensity period
                       (start-all children) '() 0))))

(define (start-all child-specs)
  (map (lambda (spec)
         (cons (child-spec-id spec)
               (spawn-and-link spec)))
       child-specs))

(define (spawn-and-link spec)
  (let ((pid (spawn (child-spec-start-thunk spec))))
    (link pid)
    pid))

(define (supervisor-loop name children strategy intensity period
                         active-children dead-history restart-count)
  (let ((msg (raw-receive #f)))
    (cond
      ; A child exited.
      ((and (pair? msg) (eq? (car msg) '*exit*))
       (let* ((from (cadr msg))
              (reason (caddr msg))
              (new-history (prune-old dead-history period))
              (new-count (+ 1 (length new-history))))
         (cond
           ((>= new-count intensity)
            ; Restart intensity exceeded: shut down all children
            ; and exit ourselves (escalate to our supervisor).
            (shutdown-all active-children)
            (exit (self) 'shutdown))
           (else
            (let ((restarted (apply-strategy strategy children
                                             active-children from)))
              (supervisor-loop name children strategy intensity period
                               restarted
                               (cons (current-jiffy) new-history)
                               new-count))))))
      ; Synchronous control message from supervisor caller.
      ((and (pair? msg) (eq? (car msg) 'which-children))
       (let ((reply-to (cdr msg)))
         (send reply-to active-children)
         (supervisor-loop name children strategy intensity period
                          active-children dead-history restart-count)))
      ; Anything else: ignore + keep going.
      (else
       (supervisor-loop name children strategy intensity period
                        active-children dead-history restart-count)))))

(define (apply-strategy strategy specs active dead-pid)
  (case strategy
    ((one-for-one)
     ; Only restart the dead child.
     (let ((id (id-of-pid dead-pid active)))
       (replace-pid active id
         (spawn-and-link (find-spec specs id)))))
    ((one-for-all)
     ; Kill all surviving children + restart all.
     (shutdown-all (filter (lambda (kv) (not (eq? (cdr kv) dead-pid))) active))
     (start-all specs))
    ((rest-for-one)
     ; Kill children started AFTER the dead one + restart from
     ; that point.
     (let-values (((before after) (split-at-pid active dead-pid)))
       (shutdown-all after)
       (append before (start-all (specs-after specs dead-pid active)))))
    (else
     (error 'apply-strategy "unknown strategy" strategy))))

(define (shutdown-all children)
  (for-each
    (lambda (kv)
      (let ((pid (cdr kv)))
        (exit pid 'shutdown)))
    children))

(define (prune-old timestamps period-seconds)
  (let* ((now (current-jiffy))
         (cutoff (- now (* period-seconds (jiffies-per-second)))))
    (filter (lambda (t) (> t cutoff)) timestamps)))

(define (id-of-pid pid active)
  (cond
    ((null? active) #f)
    ((eq? (cdar active) pid) (caar active))
    (else (id-of-pid pid (cdr active)))))

(define (find-spec specs id)
  (cond
    ((null? specs) (error 'find-spec "no spec" id))
    ((eq? (child-spec-id (car specs)) id) (car specs))
    (else (find-spec (cdr specs) id))))

(define (replace-pid active id new-pid)
  (map (lambda (kv)
         (if (eq? (car kv) id)
             (cons id new-pid)
             kv))
       active))

(define (split-at-pid active dead-pid)
  ; Returns (values before-list after-list) at the dead pid.
  ; `before` excludes the dead pid; `after` is the tail past
  ; (and not including) the dead pid.
  (let loop ((rest active) (before '()))
    (cond
      ((null? rest) (values (reverse before) '()))
      ((eq? (cdar rest) dead-pid)
       (values (reverse before) (cdr rest)))
      (else (loop (cdr rest) (cons (car rest) before))))))

(define (specs-after specs dead-pid active)
  ; Helper for rest-for-one: the specs starting AT the dead
  ; child (so we restart it + everything after).
  (let ((dead-id (id-of-pid dead-pid active)))
    (let loop ((rest specs))
      (cond
        ((null? rest) '())
        ((eq? (child-spec-id (car rest)) dead-id) rest)
        (else (loop (cdr rest)))))))

(define (supervisor-which-children sup-pid)
  (call sup-pid (cons 'which-children (self))))

; ============================================================
; PART 5 — define-behavior (gen_server analogue)
; ============================================================
;
; (define-behavior <name>
;   #:init    (lambda (args)        <body returning State>)
;   #:handle-call (lambda (msg state) <returns (values Reply NewState)>)
;   #:handle-cast (lambda (msg state) <returns NewState>)
;   #:handle-info (lambda (msg state) <returns NewState>)
;   #:terminate   (lambda (reason state) <cleanup>)
;   #:code-change (lambda (from-version state extra)
;                   <returns (values 'ok NewState)>))
;
; Expands to:
;   - (<name>-start args) — spawns the actor; returns its pid
;   - (<name>-call pid msg [timeout]) — synchronous RPC
;   - (<name>-cast pid msg) — fire-and-forget
;
; The expansion is straightforward; we use syntax-rules for now.

(define-syntax define-behavior
  (syntax-rules (init handle-call handle-cast handle-info terminate code-change)
    ((_ name
        #:init init-fn
        #:handle-call call-fn
        #:handle-cast cast-fn
        #:handle-info info-fn
        #:terminate term-fn
        #:code-change cc-fn)
     (begin
       (define (name-start . args)
         (spawn
           (lambda ()
             (let ((state (apply init-fn args)))
               (behavior-loop state call-fn cast-fn info-fn
                              term-fn cc-fn)))))
       (define (name-call pid msg)
         (call pid (cons 'call msg)))
       (define (name-cast pid msg)
         (send pid (cons 'cast msg)))))))

(define (behavior-loop state call-fn cast-fn info-fn term-fn cc-fn)
  (receive
    ;; '(call <reply-to> <msg>) — synchronous
    ((cons (cons reply-to (cons 'call msg)) #f)
     (let-values (((reply new-state) (call-fn msg state)))
       (send reply-to reply)
       (behavior-loop new-state call-fn cast-fn info-fn term-fn cc-fn)))
    ;; '(cast <msg>) — asynchronous
    ((cons 'cast msg)
     (let ((new-state (cast-fn msg state)))
       (behavior-loop new-state call-fn cast-fn info-fn term-fn cc-fn)))
    ;; system: code-change
    ((cons '*code-change* (cons from-version extra))
     (let-values (((ok new-state) (cc-fn from-version state extra)))
       (behavior-loop new-state call-fn cast-fn info-fn term-fn cc-fn)))
    ;; system: terminate
    ((cons '*exit* (cons _ reason))
     (term-fn reason state))
    ;; anything else: hand to handle-info
    (other
     (let ((new-state (info-fn other state)))
       (behavior-loop new-state call-fn cast-fn info-fn term-fn cc-fn)))))

; ============================================================
; PART 6 — table writer-actor pattern (the cs-table transactional
;          convention)
; ============================================================
;
; cs-table is concurrent-safe for single-key CRUD but offers no
; multi-key transactions. The idiomatic pattern: dedicate one
; actor as the table's gatekeeper; route writes through it; reads
; can bypass (they're lock-free in DashMap).
;
; (start-table-writer 'users 'set)
;   -> spawns a writer actor for table 'users
;
; (table-tx 'users (lambda (tab)
;   ;; tab is a handle that exposes get / put / delete; all calls
;   ;; serialize through the writer actor.
;   (let ((cur (tab 'get "alice" 0)))
;     (tab 'put "alice" (+ cur 1)))))

(define table-writers (make-table 'beam:table-writers 'set))

(define (start-table-writer name type)
  (make-table name type)
  (let ((writer (spawn
                  (lambda ()
                    (table-writer-loop name)))))
    (table-insert! 'beam:table-writers name writer)
    writer))

(define (table-writer-loop name)
  (receive
    ;; (cons reply-to (list 'get key default))
    ((cons reply-to (cons 'get (cons key (cons default _))))
     (let ((v (or (table-lookup name key) default)))
       (send reply-to v)
       (table-writer-loop name)))
    ;; (cons reply-to (list 'put key value))
    ((cons reply-to (cons 'put (cons key (cons value _))))
     (table-insert! name key value)
     (send reply-to 'ok)
     (table-writer-loop name))
    ;; (cons reply-to (list 'delete key))
    ((cons reply-to (cons 'delete (cons key _)))
     (let ((removed? (table-delete! name key)))
       (send reply-to removed?)
       (table-writer-loop name)))))

(define (table-tx name body)
  (let ((writer (table-lookup 'beam:table-writers name)))
    (unless writer
      (error 'table-tx "no writer for table" name))
    (body
      (lambda args
        (call writer args)))))

; ============================================================
; PART 7 — define-state-migration (hot-reload state migration)
; ============================================================
;
; (define-state-migration my-module
;   ((from-version "1.0") state)
;   <body producing new state>)
;
; Registers a migration function on the named module so when
; cs-hotreload promotes a new version, gen_server behaviors
; trigger code-change with the old version tag, and the
; per-actor state is passed through the registered migration
; to produce the new state.
;
; The migration table is itself a cs-table keyed by module name.
; The cs-hotreload Rust crate stays out of migration policy —
; it only tracks two versions of exports per module. The
; migration table is pure Scheme.

(define state-migrations (make-table 'beam:state-migrations 'set))

(define-syntax define-state-migration
  (syntax-rules ()
    ((_ module-name
        ((from-version from-ver-str) state-arg)
        body ...)
     (let ((existing (or (table-lookup 'beam:state-migrations 'module-name) '())))
       (table-insert!
         'beam:state-migrations 'module-name
         (cons
           (cons from-ver-str
                 (lambda (state-arg) body ...))
           existing))))))

(define (run-state-migration module-name from-version state)
  (let ((migrations (or (table-lookup 'beam:state-migrations module-name) '())))
    (cond
      ((assoc from-version migrations)
       => (lambda (entry) ((cdr entry) state)))
      ; No migration registered for this version pair — pass state
      ; through unchanged. Matches OTP's default behavior when
      ; code_change/3 isn't exported.
      (else state))))

; Wire into the behavior loop: when a *code-change* system
; message arrives at a behavior actor, look up the migration
; and produce new state before resuming the loop.
;
; (We don't redefine behavior-loop here — the version above
; already calls cc-fn with from-version/state/extra. Behaviors
; whose #:code-change wraps run-state-migration get the
; registered migration for free.)
;
; Example wire-up inside a behavior body:
;   #:code-change
;     (lambda (from-ver state _extra)
;       (values 'ok (run-state-migration 'counter from-ver state)))

