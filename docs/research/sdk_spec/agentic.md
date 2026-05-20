# Agentic Runtime — models, tools, agents, memory, policies, evals, multi-agent

Crate this spec creates: **`cs-agent`** (plus `cs-cap` shared with security.md).

Covers milestones **M09** (models + tools), **M10** (capabilities + policy
— see security.md), and **M11** (agents + memory + evals + multi-agent).

## The grand thesis

Crab Scheme is, uniquely well-suited for production agentic systems
because it already ships every primitive the field has converged on:

| Industry primitive | Crab substrate (already shipped) |
|--------------------|----------------------------------|
| Agent = supervised actor | `cs-actor` + `cs-supervisor` |
| Tool schema = typed validator | Phase 4 typed-arc contracts in `cs-runtime` |
| Sandboxed tool execution | `cs-sandbox-wasm` (L1 + L2) |
| Hot upgrade of agent code | `cs-hotreload` + `define-state-migration` |
| Memory (knowledge graph) | `graphiti` MCP + cs-stdlib-postgres pgvector |
| Optimizer-as-plugin | `cs-opt` pass framework |
| Eval as test | `crab test` runner |
| Durable agent workflow | `cs-workflow` (M08, built on cs-actor) |

The agentic milestone (M09-M11) is largely **plumbing the existing
substrate together with a thin model-provider layer + a few new
forms (`define-model`, `define-tool`, `define-agent`, `define-eval`,
`define-policy`)**, not building infrastructure from scratch.

## Vocabulary anchor

Use the converged industry names. From the research:

- **Agent** = LLM + system prompt + tools + memory + policies
- **Tool** = typed function callable by the model
- **Handoff** = transfer of control to another agent
- **Guardrail** = pre/post check on input/output
- **Policy** = capability decision (deny by default)
- **Skill** = bundled prompt + scripts + resources (Anthropic style)
- **Memory** = persistent agent-accessible state
- **Eval** = dataset-driven test with metric

Don't invent new names where the field has settled.

## M09 — Models + tools

### Model providers

```scheme
(define-model claude-opus
  #:provider 'anthropic
  #:id "claude-opus-4-7"
  #:max-tokens 8192
  #:thinking '(adaptive #:budget 16k)
  #:cache 'ephemeral-5m)

(define-model gpt-mini
  #:provider 'openai
  #:id "gpt-5-mini"
  #:max-tokens 4096)

(define-model local-llama
  #:provider 'ollama
  #:endpoint "http://localhost:11434"
  #:id "llama3:70b")
```

Providers behind features:

| Provider | Crate dep | Feature flag |
|----------|-----------|--------------|
| Anthropic | `cs-stdlib-http` | `agent-anthropic` |
| OpenAI | `cs-stdlib-http` | `agent-openai` |
| Bedrock | `aws-sdk-bedrockruntime` | `agent-bedrock` |
| Ollama / vLLM | `cs-stdlib-http` | `agent-local` |
| LiteLLM proxy | `cs-stdlib-http` | `agent-litellm` |

### The model-call loop

Each provider implements one trait:

```rust
#[async_trait]
trait ModelProvider: Send + Sync {
    async fn complete(&self, req: CompletionRequest)
        -> Result<CompletionResponse, Error>;
    fn supports(&self, capability: Capability) -> bool;
        // ::ParallelTools, ::ExtendedThinking, ::PromptCaching, etc.
}
```

The cross-provider loop (Anthropic-shaped, since that's the most
expressive and OpenAI's is a strict subset):

1. Send messages + tools.
2. If `stop_reason == "tool_use"`: extract each `tool_use` block,
   validate input against the tool's contract, dispatch the
   handler, return `tool_result` blocks.
3. If `stop_reason == "end_turn"`: return the assistant message.
4. Loop. Honor max-iteration budget.

Reference: <https://platform.claude.com/docs/en/agents-and-tools/tool-use/how-tool-use-works>

### Tools (the big one)

Tool schemas are **Crab Scheme contracts** (Phase 4 typed-arc, already shipped),
lowered to JSON Schema for the wire. This is the Scheme advantage:
schemas are real validators with descriptive error messages, server-side
and client-side.

```scheme
(define-contract send-email/in
  (object
    (to       : (and/c string? email?))
    (subject  : (and/c string? (length-at-most/c 200)))
    (body     : (and/c string? (length-at-most/c 50000)))
    (cc       : (listof email?) #:default '())))

(define-contract send-email/out
  (object (id : string?)
          (status : (or/c 'queued 'sent))))

(register-tool! 'send-email
  #:input       send-email/in
  #:output      send-email/out
  #:description "Send an email through the corporate SMTP relay."
  #:effects     '(net audit)
  #:policies    (list email-policy)
  #:handler     smtp-send)
```

The `#:effects` annotation (M01) gates tool calls through the
policy DSL (cs-cap, M10). The `audit` effect means every call is
written to the audit log regardless of policy decision.

#### Parallel tool calls

Default on. The provider's `tool_use` block list may contain
multiple calls; the runtime dispatches them in parallel via
`cs-actor` (one actor per call). Tool result order matches call
order in the response.

#### Streaming partial tool args

Behind a flag (`#:stream-partial-args #t`). Allows dispatch to
start before full JSON arrives. Only enable when the dispatcher is
idempotent. Reference: <https://andyjakubowski.com/engineering/handling-invalid-json-in-anthropic-fine-grained-tool-streaming>

#### MCP integration

Tools can be sourced from MCP servers:

```scheme
(define mcp-tools
  (mcp-connect "stdio"
               #:command "/usr/local/bin/some-mcp-server"))

(define-agent reviewer
  #:model claude-opus
  #:tools (append (list read-file run-tests)
                  (mcp-tools)))     ; ← list of Tool values
```

MCP spec: <https://modelcontextprotocol.io/specification/2025-11-25>.
Crab Scheme can also act as an MCP server — any `(register-tool! …)` can be exposed via `crabscheme mcp-server start` (CLI helper).

### v1 minimum (M09)

- `ModelProvider` trait + Anthropic / OpenAI / Ollama implementations.
- `(define-model …)` and `(define-tool …)` forms.
- Tool schema = contract, JSON-Schema lowering.
- Parallel tool calls + ephemeral prompt cache.
- MCP client (stdio + HTTP+SSE transports).
- Defer: streaming partial args; computer-use tool; programmatic tool calling; Anthropic Skills.

## M10 — Capabilities + policy DSL (cs-cap)

See `security.md` for the full design. Summary here for completeness:

```scheme
(define-policy filesystem-policy
  (deny tool-call
    #:when (lambda (call ctx)
             (or (not (path-under? (call-arg call 'path)
                                   (workspace ctx)))
                 (sensitive-file? (call-arg call 'path))))))

(define-policy production-safety
  (deny tool-call
    #:when '(and (= env "prod")
                 (in (call-tool call) '(delete-db restart-cluster))
                 (not (human-approved? call)))))

(define-agent prod-agent
  #:tools    (list shell read-file)
  #:policies (list filesystem-policy production-safety))
```

Policies run **before** the tool handler. A `deny` raises a
`&policy-denied` condition that gets serialized as a `tool_result`
error back to the model — the model sees it and can revise.

## M11 — Agents + memory + evals + multi-agent

### `define-agent`

```scheme
(define-agent support-agent
  #:model         claude-sonnet
  #:system        "You answer customer questions politely."
  #:tools         (list kb-search ticket-update)
  #:memory        support-memory
  #:policies      (list pii-guardrail)
  #:max-turns     8
  #:supervisor    (one-for-one #:max-restarts 3 #:within-ms 60000))

(define support-agent-actor (spawn-agent support-agent))
(send support-agent-actor '(query "How do I reset my password?"))
```

Under the hood: `spawn-agent` returns a `cs-actor` Pid; the actor's
behavior is the agent loop. Hot upgrade via `cs-hotreload` is free.
Failures escalate through the supervisor as for any other actor.

### Memory

Five layers, all behind one `(define-memory …)` interface:

```scheme
(define-memory support-memory
  #:vector    (pgvector-store "embeddings"
                              #:dim 1536
                              #:hybrid-bm25? #t)
  #:episodic  (sqlite-episodic "episodes.db")
  #:kg        (graphiti-store "bolt://localhost:7687")
  #:cache     (in-memory-cache #:size 1000)
  #:tenant-key 'user-id)

(memory-add! support-memory
  #:kind 'episode
  #:agent 'planner
  #:content "User prefers terse responses."
  #:importance 0.7)

(memory-search support-memory
  "what does the user want for tone?"
  #:top-k 5
  #:weights '(recency 0.3 importance 0.5 relevance 0.2))
```

Backends:

| Layer | Backend | Notes |
|-------|---------|-------|
| Vector | `pgvector` | hybrid BM25 + dense via SPARSE_INVERTED_INDEX |
| Vector | `qdrant` | OSS speed leader |
| Vector | `chroma` | DX, dev/test |
| Episodic | `sqlite` | local; weighted retrieval (recency × importance × relevance) |
| Episodic | `cs-table::OrderedSet` | embedded; no external dep |
| KG | `graphiti` MCP | knowledge graph with validity windows |
| Cache | `cs-arena` | in-process, bounded |

**Tenant isolation is mandatory** — every memory primitive takes
`#:tenant-key`. Memory leakage across tenants is the #1 production
bug.

References:
- Letta / MemGPT — <https://vectorize.io/articles/mem0-vs-letta>
- Zep / Graphiti paper — <https://arxiv.org/abs/2501.13956>
- Park et al. weighted retrieval — <https://arxiv.org/abs/2304.03442>
- Hybrid BM25 + vector — <https://medium.com/@pbronck/better-rag-accuracy-with-hybrid-bm25-dense-vector-search-ea99d48cba93>

### Durable agent workflows

Combine M08 (durable workflows) + M11 (agents):

```scheme
(define-agent-workflow resolve-incident
  (lambda (incident-id)
    (define facts
      (await (agent-step planner-agent
                         `(gather-facts ,incident-id))))
    (define plan
      (await (agent-step planner-agent
                         `(make-plan ,facts))))
    (await-human
      #:reason "Approve runbook for incident"
      #:context plan
      #:timeout (minutes 15)
      #:on-timeout 'deny)
    (await (agent-step executor-agent
                       `(execute ,plan)))))
```

Every `(agent-step …)` is an activity from M08's POV — LLM outputs
are journaled once on first execution; replays reuse the recorded
output. Token cost happens once even across hours-long pauses.

### Multi-agent patterns

Seven canonical patterns, all sugar over `cs-actor`:

```scheme
;; Supervisor / delegate
(define-supervisor-agent research-supervisor
  #:workers (list searcher summarizer cross-checker)
  #:route   (lambda (task) ...)
  #:reduce  (lambda (results) ...))

;; Pipeline
(define-pipeline content-pipeline
  (list researcher writer editor publisher))

;; Map-reduce
(define-mapreduce per-file-review
  #:map    review-one-file
  #:reduce aggregate-findings
  #:inputs (changed-files))

;; Debate
(define-debate climate-debate
  #:agents  (list optimist pessimist)
  #:judge   synthesizer
  #:rounds  3)

;; Swarm (direct handoff, no router)
(define-swarm dev-swarm
  (list planner coder critic)
  #:handoff-via (lambda (msg from-agent) ...))

;; Blackboard (shared CRDT scratchpad)
(define-blackboard dev-board
  (list planner coder critic)
  #:scratch (crdt/causal-map))

;; Human-in-the-loop
(define-agent payment-processor
  ...
  #:on-tool-call
  (lambda (call)
    (when (and (eq? (call-tool call) 'send-payment)
               (> (call-arg call 'amount) 1000))
      (await-human #:reason "Over $1k payment"))))
```

References:
- 2026 taxonomy — <https://www.digitalapplied.com/blog/agent-architecture-patterns-taxonomy-2026>
- LangGraph supervisor vs swarm — <https://focused.io/lab/multi-agent-orchestration-in-langgraph-supervisor-vs-swarm-tradeoffs-and-architecture>
- Multi-agent debate (Du et al.) — <https://hungleai.substack.com/p/agree-or-disagree-a-review-of-multi>
- Park et al. Generative Agents — <https://arxiv.org/abs/2304.03442>
- Sakana AI Scientist — <https://sakana.ai/ai-scientist/>
- CodeCRDT — <https://arxiv.org/pdf/2510.18893>

### Evals

```scheme
(define-dataset qa-golden
  #:source "datasets/qa-gold.jsonl"
  #:schema
    (object (question : string?)
            (expected : string?)
            (tags     : (listof string?))))

(define-metric llm-judge-correctness
  #:judge claude-sonnet
  #:rubric "Is the answer factually equivalent to expected? Score 0-1.")

(define-eval rag-quality
  #:agent      rag-agent
  #:dataset    qa-golden
  #:metric     llm-judge-correctness
  #:threshold  0.85
  #:trace?     #t)

(define-eval rag-trajectory
  #:agent   rag-agent
  #:dataset qa-golden
  #:metric  (trajectory-evaluator
              #:must-call     '(retrieve generate-answer)
              #:must-not-call '(send-email))
  #:threshold 1.0)
```

`(define-eval …)` integrates with the existing `crab test` runner.
Mean below `#:threshold` fails CI. Production-trace replay (sampling
from cs-stdlib-otel traces) is v1.1.

References:
- LangSmith Evaluation — <https://www.langchain.com/langsmith/evaluation>
- LangChain agentevals — <https://github.com/langchain-ai/agentevals>
- OpenAI Evals — <https://datanorth.ai/blog/evals-openais-framework-for-evaluating-llms>

### v1 minimum (M11)

- `(define-agent …)`, `(define-memory …)`, `(define-eval …)`,
  `(define-policy …)`.
- Five memory layers (vector, episodic, KG, cache, CRDT — last is M05 dependency).
- Five multi-agent patterns (supervisor, pipeline, map-reduce, swarm, HITL).
- Trajectory + LLM-as-judge metrics.
- OTel trace export.
- Defer: Skills (Anthropic-style packaged prompt+resources); debate as primitive
  (express via supervisor); CRDT-memory of v2-style multi-agent; production-trace
  sampling.

## External references (consolidated)

### Claude / Anthropic
- Building agents with Claude Agent SDK — <https://www.anthropic.com/engineering/building-agents-with-the-claude-agent-sdk>
- Tool use overview — <https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview>
- Advanced tool use — <https://www.anthropic.com/engineering/advanced-tool-use>
- Writing effective tools for AI agents — <https://www.anthropic.com/engineering/writing-tools-for-agents>
- Agent Skills — <https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills>
- Prompt caching — <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>
- Extended thinking — <https://platform.claude.com/docs/en/build-with-claude/extended-thinking>
- Constitutional AI — <https://www-cdn.anthropic.com/7512771452629584566b6303311496c262da1006/Anthropic_ConstitutionalAI_v2.pdf>
- MCP spec — <https://modelcontextprotocol.io/specification/2025-11-25>

### Frameworks
- LangGraph — <https://docs.langchain.com/oss/python/langgraph/persistence>
- AutoGen v0.4 — <https://devblogs.microsoft.com/autogen/autogen-reimagined-launching-autogen-0-4/>
- DSPy — <https://dspy.ai/>
- OpenAI Agents SDK — <https://openai.github.io/openai-agents-python/>
- Pydantic AI — <https://ai.pydantic.dev/>
- CrewAI — <https://docs.crewai.com/en/concepts/agents>
- Inngest AgentKit — <https://agentkit.inngest.com/overview>
- Mastra — <https://www.generative.inc/mastra-ai-the-complete-guide-to-the-typescript-agent-framework-2026>

### Memory
- Letta / Mem0 / MemGPT 2026 — <https://tokenmix.ai/blog/ai-agent-memory-mem0-vs-letta-vs-memgpt-2026>
- Zep / Graphiti — <https://arxiv.org/abs/2501.13956>
- Park et al. — <https://arxiv.org/abs/2304.03442>
- ACON context compression — <https://arxiv.org/pdf/2510.00615>
- ReSum — <https://arxiv.org/pdf/2509.13313>

### Multi-agent
- 2026 taxonomy — <https://www.digitalapplied.com/blog/agent-architecture-patterns-taxonomy-2026>
- LangGraph supervisor vs swarm — <https://focused.io/lab/multi-agent-orchestration-in-langgraph-supervisor-vs-swarm-tradeoffs-and-architecture>
- Multi-agent debate review — <https://hungleai.substack.com/p/agree-or-disagree-a-review-of-multi>
- Sakana AI Scientist v2 — <https://arxiv.org/pdf/2504.08066>

### Policies, guardrails, security
- NeMo Guardrails — <https://aisecurityandsafety.org/en/tools/nemo-guardrails/>
- OWASP Top 10 Agentic 2026 — <https://www.giskard.ai/knowledge/owasp-top-10-for-agentic-application-2026>
- AWS Four Security Principles — <https://aws.amazon.com/blogs/security/four-security-principles-for-agentic-ai-systems/>
- OPA + AI agents — <https://codilime.com/blog/why-use-open-policy-agent-for-your-ai-agents/>

### Evals
- LangSmith Evaluation — <https://www.langchain.com/langsmith/evaluation>
- agentevals — <https://github.com/langchain-ai/agentevals>
- Gold sets pattern — <https://medium.com/@falvarezpinto/evaluation-first-ai-product-engineering-golden-sets-drift-monitoring-and-release-gates-for-llm-2c3bfb3f1e7b>

### Durable agents
- Maxim Fateev on durable + AI — <https://workos.com/blog/maxim-fateev-temporal-durable-execution-ai-agents>
- Temporal Replay 2026 — <https://temporal.io/blog/replay-2026-product-announcements>
- Idempotent AI agents — <https://www.buildmvpfast.com/blog/idempotent-ai-agent-retry-safe-patterns-production-workflow-2026>
