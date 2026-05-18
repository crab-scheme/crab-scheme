; Conformance test for `(crab)` meta — stdlib-modules iter 14.
;
; Two procedures: `(crab-list-modules)` enumerates compiled-in
; modules; `(crab-module-procedures name)` returns each module's
; registered procedure names, or #f if the module is absent.

(test-section "(crab) — crab-list-modules")

(define __mods__ (crab-list-modules))

(test-true "result is a list"
           (list? __mods__))
(test-true "list is non-empty (this build has stdlib enabled)"
           (> (length __mods__) 0))
(test-true "every entry is a string"
           (let loop ((xs __mods__))
             (cond ((null? xs) #t)
                   ((string? (car xs)) (loop (cdr xs)))
                   (else #f))))

; The umbrella `stdlib` feature enables all 27 modules (26 functional
; + meta itself is not listed). Spot-check a few representative names
; rather than asserting the full set so subset embeds can reuse the
; test if needed.
(define (mem? s xs)
  (cond ((null? xs) #f)
        ((string=? s (car xs)) #t)
        (else (mem? s (cdr xs)))))

(test-true "includes \"path\""       (mem? "path" __mods__))
(test-true "includes \"fs\""         (mem? "fs" __mods__))
(test-true "includes \"json\""       (mem? "json" __mods__))
(test-true "includes \"hash\""       (mem? "hash" __mods__))
(test-true "includes \"http\""       (mem? "http" __mods__))
(test-true "includes \"collection\"" (mem? "collection" __mods__))
(test-true "includes \"signal\""     (mem? "signal" __mods__))

; Names come back sorted alphabetically.
(test-true "list is sorted ascending"
           (let loop ((xs __mods__))
             (cond ((or (null? xs) (null? (cdr xs))) #t)
                   ((string<=? (car xs) (cadr xs)) (loop (cdr xs)))
                   (else #f))))

(test-section "(crab) — crab-module-procedures")

(define __fs-procs__ (crab-module-procedures "fs"))

(test-true "fs returns a list" (list? __fs-procs__))
(test-true "fs list non-empty" (> (length __fs-procs__) 0))
(test-true "every entry is a string"
           (let loop ((xs __fs-procs__))
             (cond ((null? xs) #t)
                   ((string? (car xs)) (loop (cdr xs)))
                   (else #f))))

(test-true "fs includes read-file-string"
           (mem? "read-file-string" __fs-procs__))
(test-true "fs includes write-file-string"
           (mem? "write-file-string" __fs-procs__))

; `(crab path)` registers the path-* procedures.
(define __path-procs__ (crab-module-procedures "path"))
(test-true "path returns a non-empty list"
           (and (list? __path-procs__) (> (length __path-procs__) 0)))

(test-section "(crab) — error paths")

; Unknown module → #f, not an error.
(test-equal "unknown module returns #f"
            #f
            (crab-module-procedures "no-such-module"))

(test-true "non-string arg raises"
           (guard (e (#t #t)) (crab-module-procedures 42) #f))

(test-true "extra args to list raises"
           (guard (e (#t #t)) (crab-list-modules "bogus") #f))
