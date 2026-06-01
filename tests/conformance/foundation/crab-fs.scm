; Conformance test for `(crab fs)` — stdlib-modules iter 2.
; Exercises the filesystem surface against /tmp.

(test-section "(crab fs) — round-trip")

(define __tmp-string-path__ "/tmp/__crab-fs-conformance-string.txt")
(define __tmp-bytes-path__  "/tmp/__crab-fs-conformance-bytes.bin")
(define __tmp-renamed__     "/tmp/__crab-fs-conformance-renamed.txt")
(define __tmp-copied__      "/tmp/__crab-fs-conformance-copied.txt")
(define __tmp-dir__         "/tmp/__crab-fs-conformance-dir")
(define __tmp-deep-dir__    "/tmp/__crab-fs-conformance-dir/nested/deep")

; --- string round-trip ---

(write-file-string __tmp-string-path__ "hello, stdlib!")
(test-true  "file-exists? after write"  (file-exists?    __tmp-string-path__))
(test-equal "read-file-string round-trip"
            "hello, stdlib!"
            (read-file-string __tmp-string-path__))
(test-equal "file-size matches written length"
            14
            (file-size __tmp-string-path__))

; --- append ---

(append-file-string __tmp-string-path__ " more")
(test-equal "read-file-string after append"
            "hello, stdlib! more"
            (read-file-string __tmp-string-path__))

; --- bytes round-trip ---
; build the source bytevector via mutators rather than `#vu8(...)` reader
; syntax, which the parser doesn't currently recognize.

(define __tmp-bv__ (make-bytevector 7 0))
(bytevector-u8-set! __tmp-bv__ 0 0)
(bytevector-u8-set! __tmp-bv__ 1 1)
(bytevector-u8-set! __tmp-bv__ 2 2)
(bytevector-u8-set! __tmp-bv__ 3 3)
(bytevector-u8-set! __tmp-bv__ 4 255)
(bytevector-u8-set! __tmp-bv__ 5 128)
(bytevector-u8-set! __tmp-bv__ 6 64)

(write-file-bytes __tmp-bytes-path__ __tmp-bv__)
(define __round-trip__ (read-file-bytes __tmp-bytes-path__))
(test-eqv "read-file-bytes length round-trips" 7 (bytevector-length __round-trip__))
(test-eqv "read-file-bytes byte 0" 0   (bytevector-u8-ref __round-trip__ 0))
(test-eqv "read-file-bytes byte 4" 255 (bytevector-u8-ref __round-trip__ 4))
(test-eqv "read-file-bytes byte 6" 64  (bytevector-u8-ref __round-trip__ 6))

; --- rename / copy ---

(rename-file __tmp-string-path__ __tmp-renamed__)
(test-false "source missing after rename"  (file-exists? __tmp-string-path__))
(test-true  "dest exists after rename"     (file-exists? __tmp-renamed__))

(copy-file __tmp-renamed__ __tmp-copied__)
(test-true  "source still exists after copy" (file-exists? __tmp-renamed__))
(test-true  "dest exists after copy"         (file-exists? __tmp-copied__))

; --- delete ---

(delete-file __tmp-renamed__)
(delete-file __tmp-copied__)
(delete-file __tmp-bytes-path__)
(test-false "renamed deleted" (file-exists? __tmp-renamed__))
(test-false "copied deleted"  (file-exists? __tmp-copied__))
(test-false "bytes deleted"   (file-exists? __tmp-bytes-path__))

; --- directories ---

(test-section "(crab fs) — directories")

(directory-create-all __tmp-deep-dir__)
(test-true "directory-create-all creates leaf" (directory-exists? __tmp-deep-dir__))
(test-true "directory-create-all creates root" (directory-exists? __tmp-dir__))

; List the parent should contain "nested".
(test-true "directory-list contains created subdir"
           (let loop ((names (directory-list __tmp-dir__)))
             (cond ((null? names) #f)
                   ((equal? (car names) "nested") #t)
                   (else (loop (cdr names))))))

; Clean up bottom-up.
(directory-delete __tmp-deep-dir__)
(directory-delete "/tmp/__crab-fs-conformance-dir/nested")
(directory-delete __tmp-dir__)
(test-false "directory-delete leaves no trace" (directory-exists? __tmp-dir__))

; --- negative cases ---

(test-section "(crab fs) — errors")

(test-false "file-exists? on missing file"
            (file-exists? "/tmp/__definitely-missing-crab-fs-conformance__"))
(test-false "directory-exists? on missing dir"
            (directory-exists? "/tmp/__definitely-missing-crab-fs-conformance__"))

(test-section "(crab fs) — temp / glob / tree ops")
(test-true "temp-dir is a non-empty string" (> (string-length (temp-dir)) 0))

(define __td__ (make-temp-dir))
(test-true "make-temp-dir creates a directory" (directory-exists? __td__))

(write-file-string (string-append __td__ "/a.txt") "A")
(write-file-string (string-append __td__ "/b.txt") "B")
(write-file-string (string-append __td__ "/c.dat") "C")
(test-equal "glob matches by extension" 2 (length (glob (string-append __td__ "/*.txt"))))
(test-equal "glob matches everything" 3 (length (glob (string-append __td__ "/*"))))

(define __tf__ (make-temp-file))
(test-true "make-temp-file creates a file" (file-exists? __tf__))
(delete-file __tf__)

(define __dst__ (string-append __td__ "-copy"))
(copy-tree __td__ __dst__)
(test-true "copy-tree replicates the directory" (directory-exists? __dst__))
(test-equal "copy-tree copies file content" "A"
            (read-file-string (string-append __dst__ "/a.txt")))

(remove-tree __dst__)
(test-false "remove-tree deletes recursively" (directory-exists? __dst__))
(remove-tree __td__)
(test-false "remove-tree cleans up the original tree" (directory-exists? __td__))
