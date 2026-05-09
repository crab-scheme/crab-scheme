(test-section "R7RS cond-expand (library ...) clause")

; --- (library (scheme base)) is always satisfied: the runtime
;     installs all (scheme base) bindings at top level ---
(test-eqv "lib-scheme-base"
  'has-base
  (cond-expand
    ((library (scheme base)) 'has-base)
    (else                    'no-base)))

; --- (library (scheme char)) — also bundled ---
(test-eqv "lib-scheme-char"
  'has-char
  (cond-expand
    ((library (scheme char)) 'has-char)
    (else                    'no-char)))

; --- (library (scheme write)) — also bundled ---
(test-eqv "lib-scheme-write"
  'has-write
  (cond-expand
    ((library (scheme write)) 'has-write)
    (else                     'no-write)))

; --- (library (scheme time)) — also bundled ---
(test-eqv "lib-scheme-time"
  'has-time
  (cond-expand
    ((library (scheme time)) 'has-time)
    (else                    'no-time)))

; --- unknown library: falls through to else ---
(test-eqv "lib-unknown"
  'fell-through
  (cond-expand
    ((library (definitely not-a-real-lib))  'matched)
    (else                                   'fell-through)))

; --- multi-segment bogus name: also unknown ---
(test-eqv "lib-bogus-multi"
  'else-taken
  (cond-expand
    ((library (foo bar baz)) 'matched-bogus)
    (else                    'else-taken)))

; --- combined with feature test via and ---
(test-eqv "lib-and-feature"
  'both-true
  (cond-expand
    ((and (library (scheme base)) crabscheme) 'both-true)
    (else                                     'one-false)))

; --- combined with not / or ---
(test-eqv "lib-or-with-fallback"
  'has-base-or-bogus
  (cond-expand
    ((or (library (scheme base)) (library (foo))) 'has-base-or-bogus)
    (else                                         'neither)))

(test-eqv "lib-not-bogus"
  'not-bogus
  (cond-expand
    ((not (library (foo bar baz))) 'not-bogus)
    (else                          'unexpected)))

; --- nested cond-expand inside a library clause ---
(test-eqv "lib-nested"
  'inner-base
  (cond-expand
    ((library (scheme base))
     (cond-expand
       ((library (scheme char)) 'inner-base)
       (else                    'inner-other)))
    (else 'outer-other)))

; --- nested combinators ---
(test-eqv "lib-and-not"
  'matched
  (cond-expand
    ((and (library (scheme base)) (not (library (foo bar)))) 'matched)
    (else                                                    'unexpected)))
