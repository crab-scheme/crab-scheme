;;; bench/web/bench.scm
;;;
;;; TFB-style benchmark for cs-web, driven entirely from
;;; CrabScheme. Sets up the server via `web-server-create` /
;;; `web-route-static!` / `web-server-start`, drives load via
;;; `(http-get ...)` (sync, ureq-backed from cs-stdlib-http),
;;; reports per-route RPS + mean latency + p99 latency.
;;;
;;; Modeled on https://www.techempower.com/benchmarks/#section=data-r23
;;; Plaintext + JSON scenarios. Single-client sequential
;;; measurement — not aiming for absolute TFB-leader RPS (which
;;; uses 16-deep HTTP/1.1 pipelining via wrk against dedicated
;;; hardware); the goal is the RELATIVE cost of cs-web's
;;; routing / layer / actor paths in pure CrabScheme.
;;;
;;; Run:
;;;
;;;   cargo run --release -p cs-cli --features web -- bench/web/bench.scm
;;;
;;; Reads optional iteration count from REQUESTS_PER_ROUTE env-
;;; equivalent: change the (define iterations N) line below.

(define iterations 2000)

;; --- 1. Build the server (Scheme-driven from end to end). ---

(define sid (web-server-create "127.0.0.1:0"))

;; plain — Rust static route, no layers
(web-route-static! sid 'GET "/plain" "Hello, World!")

;; plain-l2 — same path body but wrapped in two Rust Layers
;; (request-id + timeout). Trace is omitted because its stderr
;; writes dominate wall-clock on loopback and we'd be measuring
;; println! throughput, not the layer overhead.
(web-layer-request-id! sid)
(web-layer-timeout! sid 5000)
(web-route-static! sid 'GET "/plain-l2" "Hello, World!")

;; json — pre-encoded JSON body. Same path mechanics as plain
;; but the response carries an application/json header (set by
;; web-respond! when status + body are passed; we use a static
;; route here for the same effect).
(web-route-static! sid 'GET "/json" "{\"message\":\"Hello, World!\"}")

(define bound (web-server-start sid))
(display "server bound at ")
(display bound)
(newline)

;; Settle the accept loop before we start hammering it.
(sleep-ms 100)

;; --- 2. Helpers --------------------------------------------------

(define (url-for path)
  (string-append "http://" bound path))

;; One request, returns (status . elapsed-ns).
(define (timed-get url)
  (let* ((t0 (monotonic-time-ns))
         (resp (http-get url))
         (t1 (monotonic-time-ns)))
    (cons (cdr (assoc "status" resp))
          (- t1 t0))))

;; Run n sequential requests against url, return (success-count
;; total-ns ordered-latencies-list).
(define (bench-loop url n)
  (let loop ((i 0)
             (oks 0)
             (lats '()))
    (if (= i n)
        (list oks
              (apply + lats)
              (list-sort < lats))
        (let* ((rv (timed-get url))
               (status (car rv))
               (lat (cdr rv)))
          (loop (+ i 1)
                (if (= status 200) (+ oks 1) oks)
                (cons lat lats))))))

(define (mean lats)
  (if (null? lats)
      0
      (quotient (apply + lats) (length lats))))

(define (percentile sorted-lats p)
  (let ((n (length sorted-lats)))
    (if (zero? n)
        0
        (list-ref sorted-lats (min (- n 1)
                                   (quotient (* n p) 100))))))

(define (rps total-ns count)
  (if (zero? total-ns)
      0
      (quotient (* count 1000000000) total-ns)))

(define (run-scenario name path)
  (let* ((url (url-for path))
         ;; Warm up — three requests, discard timings, so the
         ;; first measured request doesn't pay TCP-slow-start /
         ;; first-cache-miss cost.
         (_ (begin (http-get url) (http-get url) (http-get url)))
         (rv (bench-loop url iterations))
         (oks (car rv))
         (total (cadr rv))
         (sorted-lats (caddr rv)))
    (display name) (display "  ")
    (display "ok=") (display oks)
    (display "/") (display iterations)
    (display "  rps=") (display (rps total iterations))
    (display "  mean=") (display (quotient (mean sorted-lats) 1000)) (display "us")
    (display "  p50=") (display (quotient (percentile sorted-lats 50) 1000)) (display "us")
    (display "  p99=") (display (quotient (percentile sorted-lats 99) 1000)) (display "us")
    (newline)))

;; --- 3. Run -----------------------------------------------------

(display "cs-web TFB-style bench (driven from CrabScheme)") (newline)
(display "iterations per scenario: ") (display iterations) (newline)
(display "----------------------------------------------------------------") (newline)

(run-scenario "plain   " "/plain")
(run-scenario "plain-l2" "/plain-l2")
(run-scenario "json    " "/json")

(web-server-stop sid)
(display "stopped.") (newline)
