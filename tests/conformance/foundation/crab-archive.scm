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

;; ---- cleanup ----
(rm-rf __tmp__)
