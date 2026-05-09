(test-section "R7RS current-error-port")

; --- returns a port ---
(define ep (current-error-port))
(test-true "cep-is-port"        (port? ep))
(test-true "cep-is-output-port" (output-port? ep))

; --- subsequent calls return the same port (per dynamic extent) ---
(test-true "cep-stable"
  (eq? (current-error-port) (current-error-port)))

; --- error output port can be written to ---
(define ep2 (current-error-port))
(write-string "error: " ep2)
(write-string "something" ep2)
(test-equal "cep-collects-writes"
  "error: something"
  (get-output-string ep2))

; --- writes accumulate across multiple writes (after get clears) ---
; get-output-string clears the buffer (R6RS / our impl), so subsequent
; writes start fresh.
(write-string "next" ep2)
(test-equal "cep-after-clear" "next" (get-output-string ep2))

; --- error port distinct from output port ---
(test-false "cep-not-output-port"
  (eq? (current-error-port) (current-output-port)))

; --- error port behaves like any other output port for write-char ---
(define ep3 (current-error-port))
(write-char #\E ep3)
(write-char #\R ep3)
(write-char #\R ep3)
(test-equal "cep-write-char" "ERR" (get-output-string ep3))
