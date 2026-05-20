# M11 — Agents + memory + evals + multi-agent

**Crates extended:** `cs-agent`.
**New crates:** `cs-stdlib-otel`, `cs-stdlib-wal`.
**Effort:** 5-7 iters.
**Depends on:** M05 (CRDT memory), M08 (durable agent workflows), M09 (models+tools), M10 (policies).

## Goal

`(define-agent …)` lowers to a supervised cs-actor running the
model loop. Memory layers (vector / episodic / KG / cache / CRDT)
behind one `(define-memory …)` interface. Multi-agent patterns
(supervisor, pipeline, map-reduce, debate, swarm, blackboard,
HITL) as sugar. `(define-eval …)` integrated with `crab test`.

## Acceptance

- `(define-agent foo …)` + `(spawn-agent foo)` produces a usable Pid.
- Hot reload of an agent's body migrates state via the existing cs-hotreload mechanism.
- Memory: pgvector + sqlite-episodic + graphiti KG all addressable through one API.
- A `(define-supervisor-agent …)` composing three workers responds to a task via the supervisor LLM call.
- `(define-eval …)` runs as part of `crab test`; LLM-as-judge metric uses claude-sonnet by default.
- All agent traffic emits OTel spans; durable agent workflow replay restores trace context.

## Iters

### A — `define-agent` + supervised loop

- Lower to a `cs-actor` Pid running the agent loop.
- Restart strategy via `cs-supervisor`.
- **Code:** `crates/cs-agent/src/agent.rs` + `lib/agent/prelude.scm`.

### B — `define-memory` + backends

- Trait `MemoryStore { add, search, evict }`.
- Backends: `pgvector` (via cs-stdlib-postgres), `sqlite-episodic`, `graphiti-kg` (via MCP), `cs-arena-cache`.
- Park-style weighted retrieval (recency × importance × relevance).
- Mandatory `#:tenant-key`.

### C — Durable agent workflows (M08 integration)

- `(define-agent-workflow …)` wraps each agent call as a workflow activity.
- LLM outputs journaled once; replay reuses.
- `(await-human …)` integration.

### D — Multi-agent patterns

- `(define-supervisor-agent …)`, `(define-pipeline …)`, `(define-mapreduce …)`, `(define-debate …)`, `(define-swarm …)`, `(define-blackboard …)`.
- All sugar over actor topologies + cs-channel.

### E — `define-eval`

- Wraps `crab test` discovery.
- Built-in metrics: `string=?`, `json=?`, `llm-judge`, `trajectory-evaluator`.
- Threshold-based CI gating.

### F — OTel + cs-stdlib-otel

- Span hooks for every model call, tool call, agent step, workflow activity.
- OTLP exporter.

## Example

```scheme
(define-agent support-agent
  #:model claude-sonnet
  #:system "Answer customer questions politely."
  #:tools (list kb-search ticket-update)
  #:memory support-memory
  #:policies (list pii-guardrail)
  #:max-turns 8
  #:supervisor (one-for-one #:max-restarts 3 #:within-ms 60000))

(define-memory support-memory
  #:vector (pgvector-store "embeddings" #:dim 1536 #:hybrid-bm25? #t)
  #:episodic (sqlite-episodic "episodes.db")
  #:kg (graphiti-store "bolt://localhost:7687")
  #:tenant-key 'user-id)

;; Supervisor topology:
(define-supervisor-agent research-supervisor
  #:workers (list searcher summarizer cross-checker)
  #:route (lambda (task) ...)
  #:reduce (lambda (results) ...))

;; Eval:
(define-eval rag-quality
  #:agent rag-agent
  #:dataset (load-jsonl "datasets/qa-gold.jsonl")
  #:metric (llm-judge #:judge claude-sonnet
                      #:rubric "Score 0-1 for factual equivalence.")
  #:threshold 0.85)

;; Durable agent workflow:
(define-agent-workflow resolve-incident
  (lambda (incident-id)
    (define facts (await (agent-step planner-agent
                                     `(gather-facts ,incident-id))))
    (define plan (await (agent-step planner-agent
                                    `(make-plan ,facts))))
    (await-human #:reason "Approve runbook" #:context plan
                 #:timeout (minutes 15))
    (await (agent-step executor-agent `(execute ,plan)))))
```

## External refs

- LangGraph supervisor/swarm — <https://focused.io/lab/multi-agent-orchestration-in-langgraph-supervisor-vs-swarm-tradeoffs-and-architecture>
- Generative Agents (Park et al.) — <https://arxiv.org/abs/2304.03442>
- Letta / MemGPT — <https://vectorize.io/articles/mem0-vs-letta>
- Zep / Graphiti — <https://arxiv.org/abs/2501.13956>
- LangSmith Evaluation — <https://www.langchain.com/langsmith/evaluation>
- agentevals — <https://github.com/langchain-ai/agentevals>
- Temporal AI agents — <https://temporal.io/blog/of-course-you-can-build-dynamic-ai-agents-with-temporal>

## Code pointers

- `crates/cs-actor/src/lib.rs` — agent = actor.
- `crates/cs-supervisor/src/lib.rs` — restart strategies.
- `crates/cs-hotreload/src/lib.rs` — agent code hot reload.
- `crates/cs-workflow/` (M08) — durable agent workflows.
- `crates/cs-channel/src/lib.rs` — multi-agent topologies.
- Existing `graphiti` MCP integration.
