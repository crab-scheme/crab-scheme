# LSP Server Plan — Post-1.0-rc3

> Status: **Phase 1 COMPLETE (2026-05-21)** — `cs-lsp` crate +
> `crabscheme-lsp` binary ship live parse+expand diagnostics; exit gate
> proven end-to-end. Phases 2–6 open. Predecessor: 1.0-rc3
> (`aot-hardening` complete; all 8 microbenches AOT correctly).
> Estimated duration: 3-5 weeks across six phases.
> Spec slug: `lsp-server`.
>
> **Phase 1 done:** iters 1.1 (skeleton+initialize), 1.2 (document
> cache), 1.3/1.4 (parse+expand diagnostics), 1.6 (UTF-16 span→range),
> 1.7 (stale-change guard). Commits `1711b3e`, `01d455e`, `3f7ad5d` on
> branch `feat/lsp-server`. **iter 1.5 (compile-stage diagnostics)
> DEFERRED** — needs a cs-runtime compile-only "check" API for the full
> globals snapshot, and risks false-positive "unbound variable" on
> imported/cross-file symbols (the import resolution this plan defers).
> Do it when Phase 5's workspace/import work lands.
>
> **Target outcome:** ship `crabscheme-lsp`, a Language Server
> Protocol implementation that gives editors (VS Code, Neovim,
> Emacs, Helix) a uniform interface for diagnostics, hover, go-to-
> definition, completion, and document symbols on `.scm` files —
> reusing CrabScheme's existing front-end (cs-lex, cs-parse,
> cs-expand, cs-diag) without re-implementing parsing or
> macro-expansion.

## Why an LSP server now

Tagging 1.0-rc3 with AOT-correct microbenches shifts the
remaining barrier between "interesting Scheme implementation"
and "Scheme implementation people use" from runtime correctness
to developer-tooling. The current editor story for `.scm` files
is whatever generic syntax highlighting the editor ships with —
no inline error highlighting, no go-to-definition, no auto-
completion of `define`d identifiers. That gap is fixable cheaply
because:

1. **Front-end already streams diagnostics.** `cs-parse::read_all`
   returns `Vec<Diagnostic>`; `cs-expand::Expander::expand_program`
   returns `Result<_, Diagnostic>`; `cs-vm::compile_with_globals_and_primops`
   returns `Result<_, CompileError>`. All have `cs_diag::Span`
   (file_id + byte range) attached. LSP `Diagnostic.range` wants
   line/col — `cs-diag::SourceMap` already has the line-table
   needed for the conversion.

2. **No new IR work.** Definition + completion lookups need a
   `sym → Span` map, derivable from the existing expander's
   scope analysis. The expander already tracks identifier
   binding for hygiene; we just need to expose it.

3. **Stdio JSON-RPC is small.** A minimal server (init,
   shutdown, document open/change/save, diagnostics) is ~500
   lines using `tower-lsp` (the async Rust LSP crate). All
   features extend the same skeleton.

4. **Editor integrations are write-once.** A 30-line VS Code
   extension + a 5-line Neovim `lspconfig` snippet covers ~80%
   of the user base. The actual logic lives in
   `crabscheme-lsp`.

The 1.0-rc1 / rc2 / rc3 releases shipped binaries; an LSP server
fits the same distribution channel — one more binary in the
release tarball.

## What this plan does NOT cover

- **R6RS library system completeness.** The expander handles
  `define-library` / `import` for libraries that live as files
  in the workspace, but R6RS's full library-version /
  install-path / `--I` machinery is out of scope. Workspace-
  symbol lookup will only see files the editor's workspace root
  contains.
- **Macro expansion as feedback.** "Show me what `(when x y)`
  expands to" would be valuable but requires threading expander
  trace output through LSP `textDocument/expandMacro` (a
  non-standard extension; rust-analyzer has one). Deferred.
- **Refactorings beyond rename.** Extract function, inline let,
  etc. are large work for unclear payoff. The LSP protocol has
  `textDocument/codeAction` slots for them; we leave those
  empty.
- **Debug-adapter protocol (DAP).** A separate protocol; out of
  scope.

## Architecture

### New crate: `cs-lsp`

```
crates/cs-lsp/
├── Cargo.toml          — depends on cs-parse, cs-expand, cs-diag,
│                         cs-core, cs-vm (for primop dispatch), tokio,
│                         tower-lsp, serde_json
├── src/
│   ├── lib.rs          — crate-level docs + re-exports
│   ├── server.rs       — `Backend` impl LanguageServer
│   ├── documents.rs    — per-file cache (text, parsed Datums,
│   │                     expanded CoreExpr, diagnostics)
│   ├── diagnostics.rs  — Diagnostic ↔ lsp_types::Diagnostic
│   ├── symbols.rs      — top-level + nested defines → DocumentSymbol
│   ├── hover.rs        — primop docs + binding hover
│   ├── definition.rs   — sym → declaration Span
│   ├── completion.rs   — in-scope identifiers + primops
│   ├── semantic_tokens.rs  — cs-lex tokens → LSP semantic-token deltas
│   └── builtins.rs     — primop name → docstring table
└── tests/
    ├── server_init.rs
    ├── diagnostics.rs
    └── ... (one per feature)
```

### New binary: `crabscheme-lsp`

```
crates/cs-lsp/src/bin/crabscheme-lsp.rs  — tokio main, stdio loop
```

Distributed alongside `crabscheme` in release tarballs (same
build pipeline, same target triples).

### Communication

Standard stdio JSON-RPC 2.0. `tower-lsp` handles the framing
(Content-Length headers, message dispatch, async/await). The
server is single-threaded per-document but processes documents
concurrently.

### Document lifecycle

| LSP method                       | Action                                                    |
|----------------------------------|-----------------------------------------------------------|
| `textDocument/didOpen`           | Insert into cache; parse → expand → emit diagnostics      |
| `textDocument/didChange`         | Update cached text; re-parse → expand → emit diagnostics  |
| `textDocument/didSave`           | No-op (we always have the latest text from didChange)     |
| `textDocument/didClose`          | Remove from cache; clear diagnostics                      |

Re-parse on every change is fine for files up to ~10k LoC
(cs-parse runs in microseconds for typical Scheme files).
Incremental parsing is a Phase 6 optimization, not an MVP
requirement.

### Crate dependencies

```
cs-lsp
  ├── tower-lsp        (LSP framework, async runtime integration)
  ├── tokio            (runtime)
  ├── serde_json       (JSON-RPC encoding)
  ├── cs-parse         (read_all → Vec<Datum> + Vec<ReaderError>)
  ├── cs-expand        (Expander → Result<CoreExpr, ExpandError>)
  ├── cs-vm            (compile_with_globals_and_primops → CompileError;
  │                     primop list for completion)
  ├── cs-diag          (Span, SourceMap, Diagnostic, Severity)
  ├── cs-core          (Symbol, SymbolTable, Value)
  └── cs-runtime       (Runtime::new for the primop globals snapshot,
                        mirroring cs-cli's pattern)
```

No new dependency on cs-aot or cs-jit-cranelift — the LSP only
inspects source, it doesn't compile.

## Phasing

Six phases, each shippable independently. Phase 1 is the MVP
(diagnostics + skeleton); Phases 2-5 each add a feature set.
Phase 6 is editor integrations.

### Phase 1: MVP — JSON-RPC + Diagnostics (≈1 week)

Goal: a working LSP server that publishes parse + expand
diagnostics in real time. No hover, no completion, no
go-to-def yet — just red squigglies under bad syntax.

**Iters:**

- **1.1** Crate skeleton. `cs-lsp` Cargo.toml + lib.rs +
  `bin/crabscheme-lsp.rs`. `tower_lsp::LanguageServer` impl
  with stub methods. Wire `initialize` to return
  `ServerCapabilities { textDocumentSync, … }`. Smoke test:
  start server, send `initialize`, get response.
- **1.2** Document cache. `DashMap<Url, Document>` where
  `Document { text: String, version: i32, source_id: FileId,
  diagnostics: Vec<Diagnostic> }`. `didOpen` / `didChange` /
  `didClose` mutate this. The `FileId` is allocated against a
  shared `SourceMap`.
- **1.3** Parse pipeline. On didOpen/didChange, run
  `cs-parse::read_all` and convert `Vec<ReaderError>` to
  `lsp_types::Diagnostic[]`. Test: open a file with a missing
  closing paren; assert the LSP publishes a `severity:Error`
  diagnostic at the right line/col.
- **1.4** Expand pipeline. After parse succeeds, run
  `Expander::expand_program` and convert its `Diagnostic`
  return to LSP form. Test: open a file with `(define x)`
  (define needs 2 args); expander error surfaces.
- **1.5** Compile pipeline. After expand succeeds, run
  `compile_with_globals_and_primops` with the runtime's
  globals snapshot (per iter 2.14's pattern). Surface
  `CompileError` as a diagnostic. Test: redefine a top-level
  binding without `set!`; surfaces as a diagnostic if the
  compiler emits one.
- **1.6** Span → LSP Range conversion helper. `cs-diag::Span`
  is `(file_id, byte_start, byte_end)`; LSP wants
  `(line, character)` per offset. `SourceMap` already has
  `line_starts` per file. Write `span_to_lsp_range(span, sm)
  → lsp_types::Range`. Unit-tested with UTF-8 multibyte
  source.
- **1.7** Stale-diagnostics handling. When a new
  `didChange` lands while an older parse is still pending,
  cancel the older work (or just let it complete; the cache
  ordering ensures newer text wins). Probably implementable
  via `tower_lsp`'s `CancellationToken`.

**Exit gate:** open a `.scm` file in VS Code with `cs-lsp`
configured; introduce a syntax error; the editor shows it
inline within ~100ms.

### Phase 2: Hover + Document Symbols (≈3 days)

Goal: hovering over an identifier shows what it is (primop /
user-defined / unbound); the outline view lists top-level
defines.

**Iters:**

- **2.1** `textDocument/documentSymbol`. Walk the expanded
  `CoreExpr` for top-level `(define (name ...) ...)` and
  `(define name ...)` forms. Emit `DocumentSymbol[]` with
  `kind: Function | Variable`, `range:` the whole define,
  `selectionRange:` just the name.
- **2.2** Nested defines / letrec bindings. Recurse into
  `Lambda` / `Letrec` bodies and surface inner `define`s as
  children of their parent. Test: nqueens's `(define (place
  row placed) ...)` inside `(define (nqueens n) ...)` shows
  as a child in the outline.
- **2.3** `textDocument/hover` — primop case. Maintain a
  `HashMap<&'static str, &'static str>` of primop names → R6RS
  signature/docstring. On hover, get the identifier at the
  cursor (via cs-lex retokenize at point); if it's a primop,
  return the docstring as MarkupContent.
- **2.4** `textDocument/hover` — user-binding case. Walk the
  expanded CoreExpr to find the binding's source Span; show
  `defined at line N` + a snippet of the define's source.
  Unbound identifiers show `unbound` (matches what compile-time
  error would say).
- **2.5** Hover docstring table. Populate primop docstrings
  for the ~80 builtins in `cs-runtime/src/builtins/mod.rs`.
  Auto-generation from doc comments would be nicer; manual
  table for now.

**Exit gate:** outline view shows the file's structure; hovering
over `cons` shows its R6RS signature; hovering over a user-
defined `f` shows where it's defined.

### Phase 3: Go-to-Definition + Find References (≈3 days)

Goal: F12 jumps to where a symbol is defined; Shift-F12 lists
all uses.

**Iters:**

- **3.1** Build a `sym_table: HashMap<Symbol, BindingInfo>` per
  document where `BindingInfo { defined_at: Span, scope:
  TopLevel | Local }`. Populated as the expander walks; the
  expander already does the necessary scope analysis for
  hygiene.
- **3.2** `textDocument/definition`. Get identifier at cursor;
  look up in `sym_table`; return its `defined_at` Span as an
  LSP Location. Cross-file (workspace-symbol) deferred to
  Phase 5.
- **3.3** Reference index. For each document, also collect a
  `Vec<(Symbol, Span)>` of every identifier USE site.
- **3.4** `textDocument/references`. For each reference whose
  Symbol matches the query (and resolves to the same binding
  by scope), return all use Locations.
- **3.5** Highlight-on-hover (`textDocument/documentHighlight`).
  Cheap: same machinery as references but scoped to the
  current document; LSP clients render highlights for all
  occurrences when the cursor is on an identifier.

**Exit gate:** F12 on a user-defined function name jumps to its
`(define`; Shift-F12 lists every call site.

### Phase 4: Completion + Signature Help (≈4 days)

Goal: type `cons` and Ctrl-Space shows a completion list;
typing `(map ` shows the signature.

**Iters:**

- **4.1** `textDocument/completion`. Get the prefix at cursor;
  filter the union of (a) primops, (b) document's in-scope
  identifiers, (c) special forms (`define`, `lambda`,
  `let`, etc.). Return `CompletionItem[]` with `kind:
  Function | Variable | Keyword`.
- **4.2** Completion scoping. A `let` body should only see
  bindings in scope at the cursor. Re-use the expander's
  scope-walk: walk the CoreExpr to the cursor position,
  collecting bindings introduced by enclosing `Lambda` /
  `Letrec` / `Let*` forms.
- **4.3** Completion detail. Each `CompletionItem` gets a
  `detail` (R6RS signature for primops, `defined at line N`
  for user bindings) and `documentation` (the same MarkupContent
  hover shows).
- **4.4** Snippet completions. For special forms with a
  predictable shape, return a `CompletionItem` with
  `insertText: "(let ((${1:name} ${2:value})) ${3:body})"` and
  `insertTextFormat: Snippet`. The big ones: `let`, `let*`,
  `letrec`, `lambda`, `define`, `cond`, `case`, `when`,
  `unless`.
- **4.5** `textDocument/signatureHelp`. When inside a procedure
  call (after `(name `), look up the procedure; if a primop or
  user-defined Lambda with known params, return its parameter
  list as `SignatureInformation`.

**Exit gate:** typing `(cons` shows signature help with
`(cons obj1 obj2)`; typing `(let (` triggers a snippet
completion that inserts a let-binding scaffold.

### Phase 5: Semantic Tokens + Formatting + Workspace Features (≈5 days)

Goal: rich syntax highlighting beyond what a tmLanguage grammar
can do; gofmt-style formatting on save; workspace-wide symbol
search.

**Iters:**

- **5.1** `textDocument/semanticTokens/full`. cs-lex produces
  typed tokens (Ident, Number, String, OpenParen, etc.).
  Walk the CoreExpr to refine: distinguish bound vs unbound
  identifiers, primops vs user fns, special forms. Emit per
  LSP semantic-token spec (encoded as a flat int array).
- **5.2** `textDocument/semanticTokens/range` for incremental
  updates on large files. Same logic but bounded to the
  range.
- **5.3** Scheme formatter. Write a `cs_lsp::format::format(src)
  → String` that re-emits with canonical indentation
  (Lispy: arguments aligned with the first arg; body forms
  indented two spaces). Reuse cs-parse to get the AST, walk
  it, re-emit.
- **5.4** `textDocument/formatting` and `documentRangeFormatting`.
  Wire 5.3 into the LSP handlers.
- **5.5** `workspace/symbol`. Scan all `.scm` files in the
  workspace root for top-level defines; return matches for
  the query. Cached, invalidated on file save.
- **5.6** `textDocument/rename`. For local bindings, rename
  all uses within the file. For top-level, scan the
  workspace. Verify the new name doesn't shadow a primop
  before applying.

**Exit gate:** save a `.scm` file with messy indentation; the
editor reformats it. Cmd-T (workspace symbol) finds defines
across the project.

### Phase 6: Editor Integrations + Distribution (≈3 days)

Goal: users `brew install crabscheme` and the LSP works in
their editor with one config line.

**Iters:**

- **6.1** VS Code extension scaffold. `crabscheme-vscode/`
  directory with `package.json`, `extension.ts`. Activate on
  `.scm` file; spawn `crabscheme-lsp` over stdio. Ship the
  extension to the VS Code Marketplace.
- **6.2** Neovim lspconfig snippet. Add to nvim-lspconfig's
  upstream: `lua require'lspconfig'.crabscheme.setup{}`.
  README example.
- **6.3** Emacs / Eglot. `setq eglot-server-programs '((scheme-mode
  . ("crabscheme-lsp")))`. README example.
- **6.4** Helix. `[language]` config snippet for `.helix/config.toml`.
  README example.
- **6.5** Release pipeline. Build `crabscheme-lsp` for the
  same four target triples as `crabscheme` (darwin-aarch64,
  linux-x86_64, linux-aarch64, wasm32-wasip1 — though WASM
  may need investigation for stdio). Bundle in
  `1.0-rc4-*.tar.gz`.
- **6.6** Documentation. `docs/user/lsp.md`: install steps
  per editor, troubleshooting, feature matrix per phase.

**Exit gate:** a new contributor follows the README, installs
crabscheme, opens a `.scm` file in their editor, and gets
diagnostics + hover + completion working with zero additional
config.

## Cross-phase concerns

### Performance budget

- Parse + expand + compile a 1000-LoC file: target < 50 ms
  cold, < 5 ms warm (with the document cache hit).
- Diagnostic update latency from keystroke: target < 100 ms
  P95 (LSP guideline for "interactive").
- Memory per open document: target < 1 MB (text + parsed
  Datums + expanded CoreExpr).

Profile with `cargo flamegraph` if any of these miss.

### Error recovery

cs-parse currently bails on the first lexical error. For an
LSP we want as-much-AST-as-possible to keep features working
past one error. Phase 1 iter 1.3 should investigate adding a
`read_all_with_recovery` variant that continues past errors
by skipping to the next plausible boundary (top-level paren).
Without this, a single typo blocks all diagnostics after it.

### Logging

LSP servers log to stderr (the editor displays it in the
"Output" / log pane). Use `tracing` for structured logs;
hook to a `tracing_subscriber::fmt` writer that goes to
stderr. The `CRABSCHEME_LSP_LOG` env var sets the level
(default: `info`).

### Server modes

`crabscheme-lsp` defaults to stdio. Add a `--socket <port>`
mode for editor debugging (Helix sometimes prefers this).
Add a `--tcp` mode for remote development (less common, but
trivially supported by tower-lsp).

### Workspace configuration

LSP `workspace/configuration` lets the editor send settings.
Expose:

- `crabscheme.maxFileSize` (skip files > N KB; default 1 MB)
- `crabscheme.diagnostic.expandPhase` (`always` | `on-save` |
  `never`; default `always`)
- `crabscheme.format.indentStyle` (`lisp` | `kawa`; default
  `lisp`)
- `crabscheme.completion.snippets` (bool; default `true`)

## Risks + open questions

1. **`tower-lsp` vs hand-rolled JSON-RPC.** `tower-lsp` is the
   ecosystem standard but adds tokio + tower + several other
   deps. Tradeoff: faster MVP vs leaner dep tree. Default to
   `tower-lsp` for Phase 1; revisit if compile time bothers
   us.

2. **`textDocument/semanticTokens` payload size.** For very
   large files (>10k LoC), the encoded token array can exceed
   1 MB. LSP supports `semanticTokens/full/delta` but it's a
   pain to implement correctly. Phase 5 iter 5.2 may need to
   gate semantic-tokens on file size.

3. **Hygiene model for go-to-definition.** R6RS hygienic
   macros mean a `let` inside a macro expansion binds an
   identifier whose name might match an outer binding but
   refer to a fresh sym. Phase 3's definition lookup needs to
   honor the post-expansion scope, not the surface-syntax
   scope. The cs-expand crate already tracks the scope set;
   we just need to expose it.

4. **Cross-file completion before workspace/symbol lands.**
   Phase 4 completion would ideally include defines from
   `import`ed libraries, but the import resolution lives in
   cs-expand which is single-file today. Workspace-wide import
   resolution is a separate problem (Phase 5 iter 5.5 partially
   addresses it but only for symbol search, not full
   expansion).

5. **Macro expansion as a hover/code-lens feature.** Showing
   "this expands to" requires capturing the expander's
   intermediate output. The expander currently throws this
   away. A `CRABSCHEME_LSP_TRACE` env var that records
   expansions for the LSP to query later could work but is
   research-grade. Deferred.

6. **WASM target for the LSP binary.** stdio doesn't have a
   straightforward analog in wasm32-wasip1. Phase 6 iter 6.5
   may drop WASM from the LSP target list; that's fine since
   the editors that consume LSP all run on native.

## Reference points

- `tower-lsp` docs: https://docs.rs/tower-lsp/
- LSP 3.17 spec: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/
- rust-analyzer (Rust LSP for Rust) — model for many of these
  features: https://github.com/rust-lang/rust-analyzer
- racket-langserver — the closest existing Scheme LSP:
  https://github.com/jeapostrophe/racket-langserver
- Inspiration for `expandMacro` extension: rust-analyzer's
  `rust-analyzer/expandMacro`.

## Success metrics

- **Coverage**: every LSP method in this plan responds (even
  if some return empty results).
- **Correctness**: a maintained test suite — one round-trip
  test per feature (open file, fire method, assert response).
- **Latency**: P95 diagnostic latency < 100 ms on the
  cs-runtime/tests/conformance/ corpus (~3000 lines of
  Scheme).
- **Adoption**: at least one user-confirmed working setup in
  each of the four editors (VS Code, Neovim, Emacs, Helix)
  before the LSP ships in a release tarball.
