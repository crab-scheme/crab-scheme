# CrabScheme MCP server — AI-agent integration

CrabScheme ships `crabscheme-mcp`, an [MCP][mcp-spec] server that exposes
the same code-intelligence the editor LSP exposes — but in a shape any
MCP client (Claude Code, Claude Desktop, ChatGPT MCP, custom agents) can
call. The agent gets first-class access to your Scheme code: parse-time
diagnostics, symbol outlines, hover info, definitions, references,
formatting, and workspace-wide symbol search.

The same harness drives all three surfaces (CLI, LSP, MCP), so an agent
can never "see something different" from what your editor sees.

[mcp-spec]: https://spec.modelcontextprotocol.io/

## Why this matters

The strongest practical pitch for CrabScheme today is that it's the
**first Scheme with first-class AI-agent integration** out of the box.
You don't have to write or maintain glue code; the MCP server speaks
the standard protocol every modern LLM dev tool understands.

The validated spec version is **MCP 2025-06-18**; no MUST violations.
Round-trip tested in `crates/cs-lsp/tests/mcp_e2e.rs`.

## Quickstart — Claude Code

If you're working inside the CrabScheme repo, the `.mcp.json` in the
repo root wires the server in automatically — Claude Code picks it up
on session start.

For your own Scheme project, install the binary and add an entry to
your `~/.claude.json` (or project-local `.mcp.json`):

```sh
cargo install --path /path/to/crab-scheme/crates/cs-lsp   # → crabscheme-mcp
```

```jsonc
// ~/.claude.json or .mcp.json
{
  "mcpServers": {
    "crabscheme": { "command": "crabscheme-mcp" }
  }
}
```

That's it. Open Claude Code in a directory with `.scm` files and ask:

> "What's the symbol outline of `lib/beam/prelude.scm`?"

> "Find all references to `make-supervisor` in this codebase."

> "Are there any parse errors in `src/main.scm`?"

## Quickstart — Claude Desktop

Same idea, but Claude Desktop doesn't search `$PATH` — you need the
absolute path:

```jsonc
// ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "crabscheme": {
      "command": "/Users/you/.cargo/bin/crabscheme-mcp"
    }
  }
}
```

Restart Claude Desktop. The 7 tools below appear under the
crabscheme server.

## The 7 tools

Each tool takes a file path (and sometimes a 1-based position); each
returns JSON the agent can reason over.

| Tool | What it returns |
|---|---|
| `cs_diagnostics` | Parse + macro-expansion errors with source spans |
| `cs_symbols` | Document symbol outline (defines, lambdas, etc.) |
| `cs_definition` | Jump-to-definition for an identifier at a position |
| `cs_references` | All textual references to an identifier |
| `cs_hover` | Hover info (kind + summary) for the identifier at a position |
| `cs_format` | Pretty-printed source |
| `cs_workspace_symbols` | Full-text symbol search across the workspace |

**Resolution caveat**: definitions, references, and rename are
name-based — they match every textual use of an identifier (including
its definition) but **do not yet honor lexical scope/hygiene**.
Diagnostics cover parse + macro-expansion errors (not yet type or
compile errors). The hygiene-aware path is tracked as deferred work in
[`docs/user/lsp.md`](lsp.md#feature-matrix).

## What you can ask the agent

Concrete patterns that work well today:

- **"Outline this file"** → `cs_symbols` gives a clean tree the agent
  can summarize.
- **"What does `X` do?"** → `cs_hover` + `cs_definition` to find and
  describe.
- **"Find every callsite of `X`"** → `cs_references`; agent can then
  read each location to understand usage patterns.
- **"Are there errors in this file?"** → `cs_diagnostics`, formatted
  spans + messages.
- **"Reformat this code"** → `cs_format` for canonical formatting.
- **"Where in the repo do I define `Y`?"** → `cs_workspace_symbols`.

Patterns to use cautiously:

- **"Rename `X` across the workspace"** — works textually but won't
  distinguish a shadowed binding; review the diff carefully or use
  the LSP `textDocument/rename` from a real editor for the
  hygiene-aware version (still name-based, but at least bound to a
  selection).

## Headless CLI alternative

For agents/scripts that shell out instead of speaking MCP, the same
harness is exposed as a sync JSON CLI:

```sh
crabscheme-lsp diagnostics src/main.scm
crabscheme-lsp symbols    src/main.scm
crabscheme-lsp definition src/main.scm 12 7      # 1-based line/col
crabscheme-lsp references src/main.scm 12 7
crabscheme-lsp hover      src/main.scm 12 7
crabscheme-lsp format     src/main.scm
crabscheme-lsp workspace-symbols my-pattern .
```

Same outputs, no MCP setup. Useful in CI and shell pipelines.

## See also

- [`docs/user/lsp.md`](lsp.md) — full LSP surface, editor setup
  (VS Code, Neovim, Emacs, Helix), and detailed feature matrix
  (LSP / CLI / MCP).
- `crates/cs-lsp/tests/mcp_e2e.rs` — end-to-end MCP lifecycle test;
  also a runnable example of every tool's request/response shape.
- `.mcp.json` at the repo root — dogfood configuration for working
  on CrabScheme itself.
