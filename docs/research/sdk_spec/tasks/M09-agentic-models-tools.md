# M09 — Agentic runtime: models + tools

**Crates created:** `cs-agent` (initial slice).
**Effort:** 3-4 iters.
**Depends on:** existing `cs-stdlib-http`, Phase 4 typed-arc contracts (already shipped).

## Goal

`(define-model …)` and `(define-tool …)` forms; multi-provider
HTTP client; tool schemas as contracts; parallel tool calls;
ephemeral prompt caching; MCP client.

## Acceptance

- Three providers compile + run: Anthropic, OpenAI, Ollama.
- Tool input validated as a contract; JSON Schema lowered to wire.
- Anthropic loop handles parallel `tool_use` blocks.
- Ephemeral cache markers honored (5-min TTL).
- MCP client connects via stdio + HTTP+SSE to a sample MCP server.

## Iters

### A — `ModelProvider` trait + Anthropic

- `trait ModelProvider { async fn complete(req) → resp }`.
- Anthropic provider: tool_use loop, parallel tools, prompt cache.
- **Code:** new `crates/cs-agent/src/providers/anthropic.rs`.

### B — OpenAI + Ollama providers

- OpenAI: Responses API with parallel function calls.
- Ollama / vLLM: local-first.

### C — `(define-tool …)` + contract lowering

- Schema = a Phase 4 contract; lowered to JSON Schema for the wire.
- Effects (M01) + policies (M10) gate the handler.
- **Code:** `crates/cs-agent/src/tool.rs`.

### D — MCP client

- Stdio + HTTP+SSE transports.
- `(mcp-connect …)` returns a list of `Tool` values.
- **Code:** `crates/cs-agent/src/mcp.rs`.

## Example

```scheme
(define-model claude-opus
  #:provider 'anthropic
  #:id "claude-opus-4-7"
  #:max-tokens 8192
  #:thinking '(adaptive #:budget 16k)
  #:cache 'ephemeral-5m)

(define-contract send-email/in
  (object (to : email?)
          (subject : (and/c string? (length-at-most/c 200)))
          (body : (and/c string? (length-at-most/c 50000)))))

(register-tool! 'send-email
  #:input        send-email/in
  #:description  "Send an email through the corporate SMTP relay."
  #:effects      '(net audit)
  #:handler      smtp-send)

;; Use an MCP server's tools:
(define filesystem-tools
  (mcp-connect "stdio"
               #:command "/usr/local/bin/mcp-filesystem-server"))
```

## External refs

- Building agents with Claude Agent SDK — <https://www.anthropic.com/engineering/building-agents-with-the-claude-agent-sdk>
- Tool use overview — <https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview>
- OpenAI function calling — <https://developers.openai.com/api/docs/guides/function-calling>
- MCP spec — <https://modelcontextprotocol.io/specification/2025-11-25>
- Writing effective tools — <https://www.anthropic.com/engineering/writing-tools-for-agents>

## Code pointers

- `crates/cs-stdlib-http/` — HTTP client.
- `crates/cs-stdlib-json/` — JSON Schema validation.
- `crates/cs-runtime/src/builtins/mod.rs` — register `define-tool`, `define-model` primops.
- Existing typed-arc Phase 4 — contracts.
