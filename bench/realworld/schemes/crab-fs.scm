; (crab fs) realworld bench — stdlib-modules iter 15.
;
; Per-iter: write a payload to a tmp file, read it back, append,
; read again, delete. Exercises read-file-string / write-file-string
; / append-file-string / delete-file / file-exists?. Result is the
; final read string length (deterministic, integer).
;
; Uses (crab path) for tmp-path joining + (crab os) for the tmp
; dir so we don't hard-code "/tmp".

(define __payload__
  (let loop ((i 0) (acc '()))
    (if (= i 50)
        (apply string-append (reverse acc))
        (loop (+ i 1) (cons "abcdefghijklmnopqrstuvwxyz0123456789\n" acc)))))

(define __addendum__ "addendum:end\n")

; Pick a stable per-process tmp path so the bench is repeatable.
(define __tmp__
  (string-append (or (get-environment-variable "TMPDIR") "/tmp")
                 "/crab-realworld-fs.tmp"))

; Clean up any stale file from a prior crashed run.
(if (file-exists? __tmp__) (delete-file __tmp__))

(realworld-bench
  "crab-fs"
  '((payload-bytes . 1850) (cycle . "write+read+append+read+delete"))
  (lambda ()
    (write-file-string __tmp__ __payload__)
    (let ((after-write (read-file-string __tmp__)))
      (append-file-string __tmp__ __addendum__)
      (let* ((after-append (read-file-string __tmp__))
             (final-len (string-length after-append)))
        (delete-file __tmp__)
        final-len))))
