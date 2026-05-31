; Conformance test for `(crab sql)` — embedded SQLite.

(test-section "(crab sql) — open / predicate")

(define db (sql-open ":memory:"))
(test-true "sql-open returns a connection" (sql-connection? db))
(test-false "a string is not a connection" (sql-connection? "db"))
(test-false "a plain vector is not a connection" (sql-connection? (vector 1 2)))

(test-section "(crab sql) — DDL + insert + ids")

(sql-execute-batch db "create table todo(id integer primary key, what text, done integer);")
(test-equal "insert reports 1 row changed"
            1 (sql-execute db "insert into todo(what, done) values (?, ?)" "buy milk" 0))
(test-equal "second insert reports 1 row changed"
            1 (sql-execute db "insert into todo(what, done) values (?, ?)" "write code" 1))
(test-equal "last-insert-id tracks the rowid" 2 (sql-last-insert-id db))

(test-section "(crab sql) — query shapes")

(define rows (sql-query db "select id, what, done from todo order by id"))
(test-equal "two rows returned" 2 (length rows))
(test-equal "row alist: id column" 1 (cdr (assoc "id" (car rows))))
(test-equal "row alist: what column" "buy milk" (cdr (assoc "what" (car rows))))
(test-equal "query-value returns a scalar" 2 (sql-query-value db "select count(*) from todo"))

(define one (sql-query-row db "select what from todo where id = ?" 2))
(test-equal "query-row returns the first match" "write code" (cdr (assoc "what" one)))
(test-false "query-row with no match is #f" (sql-query-row db "select * from todo where id = 999"))
(test-false "query-value with no match is #f"
            (sql-query-value db "select what from todo where id = 999"))

(test-section "(crab sql) — type round-trips")

(sql-execute-batch db "create table types(i integer, r real, t text, b blob, n integer);")
(sql-execute db "insert into types(i, r, t, b, n) values (?, ?, ?, ?, ?)"
             42 3.5 "hello" (string->utf8 "bytes") '())
(define tr (sql-query-row db "select i, r, t, b, n from types"))
(test-equal "integer round-trips" 42 (cdr (assoc "i" tr)))
(test-equal "real round-trips" 3.5 (cdr (assoc "r" tr)))
(test-equal "text round-trips" "hello" (cdr (assoc "t" tr)))
(test-equal "blob round-trips" "bytes" (utf8->string (cdr (assoc "b" tr))))
(test-false "SQL NULL reads back as #f" (cdr (assoc "n" tr)))

(test-section "(crab sql) — errors + close")

(test-true "malformed SQL raises"
           (guard (e (#t #t)) (sql-execute db "this is not sql") #f))

(sql-close! db)
(test-true "use after close raises"
           (guard (e (#t #t)) (sql-query db "select 1") #f))
(test-true "sql-close! is idempotent"
           (begin (sql-close! db) #t))
