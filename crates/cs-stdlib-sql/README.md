# `(crab sql)` — embedded SQLite

CrabScheme stdlib module — the `(crab …)` answer to Python's
`sqlite3`, Go's `database/sql`, and Clojure's JDBC wrappers. Backed
by [`rusqlite`] with the bundled SQLite amalgamation, so there's no
database server to install.

> **Native-only.** The `bundled` feature compiles the SQLite C
> amalgamation, so this module needs a C compiler at build time and
> is **not** part of `wasm-stdlib` (like the networking modules).

A connection is an opaque handle stored in a per-thread registry
(the runtime is single-threaded). Release it with `sql-close!`.

## Procedures

```
(sql-open path)                    ;-> connection ; ":memory:" for in-memory
(sql-close! conn)                  ;-> unspec     ; idempotent
(sql-connection? v)                ;-> boolean
(sql-execute conn sql . params)    ;-> fixnum     ; rows changed
(sql-execute-batch conn script)    ;-> unspec     ; multi-statement, no params
(sql-query conn sql . params)      ;-> list       ; rows as (col . value) alists
(sql-query-row conn sql . params)  ;-> alist | #f ; first row
(sql-query-value conn sql . params);-> value | #f ; first column of first row
(sql-last-insert-id conn)          ;-> fixnum
```

## Example

```scheme
(import (crab sql))

(define db (sql-open ":memory:"))
(sql-execute-batch db "create table todo(id integer primary key, what text);")
(sql-execute db "insert into todo(what) values (?)" "buy milk")
(sql-execute db "insert into todo(what) values (?)" "write code")

(sql-query db "select * from todo")
; => ((("id" . 1) ("what" . "buy milk")) (("id" . 2) ("what" . "write code")))

(sql-query-value db "select count(*) from todo")   ; => 2
(sql-close! db)
```

## Type mapping

| Scheme (param) | SQLite | SQLite (result) | Scheme |
|---|---|---|---|
| exact integer | INTEGER | INTEGER | fixnum |
| flonum / rational | REAL | REAL | flonum |
| string | TEXT | TEXT | string |
| bytevector | BLOB | BLOB | bytevector |
| `#t` / `#f` | INTEGER 1 / 0 | NULL | `#f` |
| `'()` / unspecified | NULL | | |

SQL `NULL` reads back as `#f`. Exact integers beyond ±2⁵³ lose
exactness when bound as parameters (they pass through an `f64`);
results always come back as full-range fixnums.

## Transactions

Run them through `sql-execute`:

```scheme
(sql-execute db "begin")
(sql-execute db "insert into todo(what) values (?)" "step 1")
(sql-execute db "insert into todo(what) values (?)" "step 2")
(sql-execute db "commit")   ; or "rollback"
```

[`rusqlite`]: https://github.com/rusqlite/rusqlite
