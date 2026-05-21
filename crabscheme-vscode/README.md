# CrabScheme for VS Code

Language support for [CrabScheme](https://github.com/crab-scheme/crabscheme)
`.scm` files, powered by the `crabscheme-lsp` language server: diagnostics,
outline, hover, go-to-definition, find references, completion, signature
help, formatting, workspace symbols, rename, and semantic highlighting.

## Prerequisites

The `crabscheme-lsp` binary must be on your `PATH` (or set
`crabscheme.serverPath`). Install it from source:

```sh
cargo install --path crates/cs-lsp   # → crabscheme-lsp (+ crabscheme-mcp) in ~/.cargo/bin
```

## Build & install the extension

This extension isn't published to the Marketplace — build the `.vsix` and
install it locally:

```sh
cd crabscheme-vscode
npm install
npm run package                   # produces crabscheme-vscode-<version>.vsix
code --install-extension crabscheme-vscode-*.vsix
```

For development, open this folder in VS Code and press `F5` to launch an
Extension Development Host.

## Settings

| Setting | Default | Description |
| --- | --- | --- |
| `crabscheme.serverPath` | `crabscheme-lsp` | Path to the language server. |
| `crabscheme.trace.server` | `off` | Trace LSP traffic (`off`/`messages`/`verbose`). |

## Notes

- Highlighting is provided by the server's semantic tokens (keyword vs.
  builtin vs. variable). A standalone TextMate grammar for pre-server
  coloring is a future enhancement.
- See [`docs/user/lsp.md`](../docs/user/lsp.md) for the other editors and
  the coding-agent (MCP/CLI) integrations.
