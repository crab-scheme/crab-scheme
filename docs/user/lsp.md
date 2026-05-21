# CrabScheme editor + agent integration

CrabScheme ships code intelligence for `.scm` files in three forms, all
built from the **same** front-end (`cs-parse` / `cs-expand` / `cs-diag`)
so they never disagree:

| Surface | Binary | Speaks | For |
| --- | --- | --- | --- |
| **LSP server** | `crabscheme-lsp` | LSP (JSON-RPC over stdio) | editors (VS Code, Neovim, Emacs, Helix) |
| **Headless CLI** | `crabscheme-lsp <cmd>` | JSON on stdout | scripts, CI, agents that shell out |
| **MCP server** | `crabscheme-mcp` | MCP (JSON-RPC over stdio) | Claude Code/Desktop & other MCP agents |

Features: diagnostics, document symbols (outline), hover, go-to-definition,
find-references, document highlight, completion, signature help, document
formatting, workspace symbol search, rename, and semantic tokens.

> Resolution is name-based: references/rename match every textual use of an
> identifier (including its definition) but do not yet honor lexical
> scope/hygiene. Diagnostics cover parse + macro-expansion errors (not yet
> type/compile errors).

## Install from source

The servers aren't published to any package registry — build them from a
checkout. The `cs-lsp` crate produces **both** `crabscheme-lsp` and
`crabscheme-mcp`, so one `cargo install` puts them on your `PATH`
(`~/.cargo/bin`):

```sh
cargo install --path crates/cs-lsp     # → crabscheme-lsp + crabscheme-mcp
cargo install --path crates/cs-cli     # → crabscheme (the interpreter), optional
```

Make sure `~/.cargo/bin` is on your `PATH` (`cargo install` prints the
location). Verify with `crabscheme-lsp --version`; smoke-test
`crabscheme-mcp` per the Troubleshooting section (it reads JSON-RPC on
stdin and has no flags).

If you'd rather not install, just build and reference the binaries by path
(`target/release/crabscheme-lsp`):

```sh
cargo build --release -p cs-lsp        # binaries land in target/release/
```

> The `wasm32-wasip1` build ships only `crabscheme.wasm`; the LSP/MCP
> servers are native-only (they need host stdio).

## Editors (LSP)

### VS Code

The extension isn't on the Marketplace — build the `.vsix` from
[`crabscheme-vscode/`](../../crabscheme-vscode/) and install it locally:

```sh
cd crabscheme-vscode && npm install && npm run package
code --install-extension crabscheme-vscode-*.vsix
```

It activates on `.scm` files and spawns `crabscheme-lsp` from your `PATH`
(override with the `crabscheme.serverPath` setting).

### Neovim (0.8+, built-in LSP)

```lua
vim.lsp.config = vim.lsp.config or {}
vim.api.nvim_create_autocmd("FileType", {
  pattern = "scheme",
  callback = function(args)
    vim.lsp.start({
      name = "crabscheme-lsp",
      cmd = { "crabscheme-lsp" },
      root_dir = vim.fs.dirname(vim.fs.find({ ".git" }, { upward = true })[1]),
    })
  end,
})
```

With `nvim-lspconfig` you can instead define a custom server with
`cmd = { "crabscheme-lsp" }` and `filetypes = { "scheme" }`.

### Emacs (Eglot, built into Emacs 29+)

```elisp
(add-to-list 'eglot-server-programs '(scheme-mode . ("crabscheme-lsp")))
(add-hook 'scheme-mode-hook #'eglot-ensure)
```

### Helix (`languages.toml`)

```toml
[language-server.crabscheme-lsp]
command = "crabscheme-lsp"

[[language]]
name = "scheme"
language-servers = ["crabscheme-lsp"]
```

## Coding agents (MCP + CLI)

### MCP server — Claude Code

A repo-local [`.mcp.json`](../../.mcp.json) is committed, so Claude Code
working **in this repository** gets the `crabscheme` MCP server
automatically (it runs `crabscheme-mcp` via `cargo run`). For your own
projects, add to the project's `.mcp.json` or your user config:

```json
{
  "mcpServers": {
    "crabscheme": { "command": "crabscheme-mcp" }
  }
}
```

### MCP server — Claude Desktop

Add to `claude_desktop_config.json` (macOS:
`~/Library/Application Support/Claude/claude_desktop_config.json`). Claude
Desktop doesn't inherit your shell `PATH` or expand `~`, so use the
**absolute** path that `cargo install` reported (typically
`/Users/<you>/.cargo/bin/crabscheme-mcp`):

```json
{
  "mcpServers": {
    "crabscheme": { "command": "/Users/you/.cargo/bin/crabscheme-mcp" }
  }
}
```

The server exposes these tools (each source tool takes a file `path` **or**
inline `text`; positions are 1-based):

| Tool | Arguments | Returns |
| --- | --- | --- |
| `cs_diagnostics` | `path`\|`text` | parse/expand errors `[{severity,message,range}]` |
| `cs_symbols` | `path`\|`text` | outline `[{name,kind,range}]` |
| `cs_definition` | `path`\|`text`, `line`, `col` | definition range or `null` |
| `cs_references` | `path`\|`text`, `line`, `col` | every reference `[range]` |
| `cs_hover` | `path`\|`text`, `line`, `col` | doc string or `null` |
| `cs_format` | `path`\|`text` | reformatted source |
| `cs_workspace_symbols` | `root`, `query` | cross-file defines `[{name,kind,path,range}]` |

### Headless CLI

For agents/scripts that shell out instead of speaking MCP:

```sh
crabscheme-lsp check  foo.scm            # diagnostics JSON; exit 1 if any
crabscheme-lsp symbols foo.scm           # outline JSON
crabscheme-lsp def   foo.scm --line 2 --col 2
crabscheme-lsp refs  foo.scm --line 2 --col 2
crabscheme-lsp hover foo.scm --line 1 --col 2
crabscheme-lsp fmt   foo.scm [--write]   # to stdout, or rewrite in place
crabscheme-lsp workspace-symbols ./src --query alpha
```

Exit codes: `0` ok, `1` `check` found diagnostics, `2` unreadable
input/args, `3` internal error.

## Feature matrix

| Feature | LSP | CLI | MCP |
| --- | :-: | :-: | :-: |
| diagnostics | ✅ | `check` | `cs_diagnostics` |
| document symbols | ✅ | `symbols` | `cs_symbols` |
| hover | ✅ | `hover` | `cs_hover` |
| go-to-definition | ✅ | `def` | `cs_definition` |
| find references | ✅ | `refs` | `cs_references` |
| document highlight | ✅ | — | — |
| completion | ✅ | — | — |
| signature help | ✅ | — | — |
| formatting | ✅ | `fmt` | `cs_format` |
| workspace symbols | ✅ | `workspace-symbols` | `cs_workspace_symbols` |
| rename | ✅ | — | — |
| semantic tokens | ✅ | — | — |

(Completion, signature help, highlight, rename, and semantic tokens are
interactive editor features, so they're LSP-only.)

## Troubleshooting

- **No diagnostics / "server not running".** Confirm `crabscheme-lsp` is
  on `PATH` (`which crabscheme-lsp`). Editors spawn it bare with no
  arguments — that's the LSP mode.
- **MCP server does nothing.** It speaks newline-delimited JSON-RPC on
  stdio; only JSON-RPC goes to stdout (logs go to stderr). Verify with:
  ```sh
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}' | crabscheme-mcp
  ```
- **Wrong positions.** LSP uses 0-based UTF-16; the CLI and MCP use
  **1-based** line/column. Don't mix them.
- **Stale results after edits.** The CLI/MCP analyze the file/text you
  pass each call — they hold no cache. The LSP re-analyzes on every
  change.
