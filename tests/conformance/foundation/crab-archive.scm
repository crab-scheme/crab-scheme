; Conformance test for `(crab archive)` — stdlib-modules iter 7.
;
; Uses (crab process) to shell out to the host `tar`/`zip` binaries
; to construct fixtures, then exercises the read/extract paths via
; cs-stdlib-archive.

(define __tmp__ "/tmp/__crab-archive-test")
(define __src__ (string-append __tmp__ "/src"))
(define __extract__ (string-append __tmp__ "/extract"))
(define __tar__ (string-append __tmp__ "/payload.tar"))
(define __tar-gz__ (string-append __tmp__ "/payload.tar.gz"))

(define (rm-rf p)
  (run "sh" (list "-c" (string-append "rm -rf " p))))

;; ---- fixture setup ----

(rm-rf __tmp__)
(directory-create-all __src__)
(write-file-string (string-append __src__ "/hello.txt") "hello\n")
(write-file-string (string-append __src__ "/numbers.txt") "1\n2\n3\n")

;; Build a tar fixture: cd src && tar -cf payload.tar .
(run "sh" (list "-c" (string-append "cd " __src__ " && tar -cf " __tar__ " .")))
;; And a tar.gz.
(run "sh" (list "-c" (string-append "cd " __src__ " && tar -czf " __tar-gz__ " .")))

(test-section "(crab archive) — tar")

(define __names__ (tar-list __tar__))
(test-true "tar-list returns a list" (pair? __names__))

;; tar -cf . puts entries like "./hello.txt" and "./numbers.txt".
;; Membership test — ordering depends on tar version.
(test-true "tar-list mentions hello.txt"
           (let loop ((rest __names__))
             (cond ((null? rest) #f)
                   ((string-contains? (car rest) "hello.txt") #t)
                   (else (loop (cdr rest))))))

(directory-create-all __extract__)
(tar-extract __tar__ __extract__)
(test-equal "tar-extract round-trips hello.txt"
            "hello\n"
            (read-file-string (string-append __extract__ "/hello.txt")))

(test-section "(crab archive) — tar.gz")

(define __gz-names__ (tar-gz-list __tar-gz__))
(test-true "tar-gz-list returns a list" (pair? __gz-names__))

(rm-rf __extract__)
(directory-create-all __extract__)
(tar-gz-extract __tar-gz__ __extract__)
(test-equal "tar-gz-extract round-trips numbers.txt"
            "1\n2\n3\n"
            (read-file-string (string-append __extract__ "/numbers.txt")))

(test-section "(crab archive) — safety")

;; tar-slip: an entry with an absolute path or `..` component must
;; be refused. Build a hostile tarball with an entry named
;; `../escape.txt` and confirm extraction raises.
(define __evil-src__ (string-append __tmp__ "/evil-src"))
(define __evil-tar__ (string-append __tmp__ "/evil.tar"))
(rm-rf __evil-src__)
(directory-create-all __evil-src__)
(write-file-string (string-append __evil-src__ "/payload") "owned\n")
(run "sh" (list "-c"
                (string-append "cd " __tmp__
                               " && tar -cf " __evil-tar__
                               " -C " __evil-src__
                               " --transform='s,payload,../escape.txt,'"
                               " payload")))

(rm-rf __extract__)
(directory-create-all __extract__)
(test-true "tar-extract refuses ../ entries"
           (guard (e (#t #t)) (tar-extract __evil-tar__ __extract__) #f))
(test-true "no file escaped the dest dir"
           (not (file-exists? (string-append __tmp__ "/escape.txt"))))

;; Symlink rejection: build a tarball containing a symlink entry.
;; Most tar binaries store symlinks verbatim; we verify extraction
;; refuses to materialize them.
(define __sym-src__ (string-append __tmp__ "/sym-src"))
(define __sym-tar__ (string-append __tmp__ "/sym.tar"))
(rm-rf __sym-src__)
(directory-create-all __sym-src__)
(run "sh" (list "-c" (string-append "ln -sf /etc/passwd " __sym-src__ "/link")))
(run "sh" (list "-c" (string-append "cd " __sym-src__ " && tar -cf " __sym-tar__ " .")))

(rm-rf __extract__)
(directory-create-all __extract__)
(test-true "tar-extract refuses symlink entries"
           (guard (e (#t #t)) (tar-extract __sym-tar__ __extract__) #f))

;; Size cap: extract with a tiny cap (1 byte). The hello.txt entry
;; (6 bytes) exceeds it, so extraction raises.
(rm-rf __extract__)
(directory-create-all __extract__)
(test-true "tar-extract refuses output exceeding max-bytes cap"
           (guard (e (#t #t)) (tar-extract __tar__ __extract__ 1) #f))

;; ---- cleanup ----
(rm-rf __tmp__)
