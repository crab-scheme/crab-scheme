# External References

Curated citation list from the four research passes that fed this
spec. Sub-headings match section headings in the per-subsystem
documents. URLs are stable as of 2026-05-19.

## Actor model & distributed runtime

### Erlang / OTP
- Erlang Distribution Protocol — <https://www.erlang.org/doc/apps/erts/erl_dist_protocol.html>
- Supervisor behaviour — <https://www.erlang.org/doc/apps/stdlib/supervisor.html>
- Processes (link/monitor) — <https://www.erlang.org/doc/system/ref_man_processes.html>
- `pg` (process groups) — <https://www.erlang.org/doc/apps/kernel/pg.html>
- Learn You Some Erlang — Supervisors — <https://learnyousomeerlang.com/supervisors>
- Alternative carriers for distribution — <https://www.erlang.org/doc/apps/erts/alt_dist.html>
- EEF distribution hardening — <https://security.erlef.org/secure_coding_and_deployment_hardening/distribution.html>

### Akka
- Cluster Specification — <https://doc.akka.io/docs/akka/current/typed/cluster-concepts.html>
- Cluster Sharding — <https://doc.akka.io/libraries/akka-core/current/typed/cluster-sharding.html>
- `LeastShardAllocationStrategy` — <https://github.com/akka/akka-core/blob/main/akka-cluster-sharding/src/main/scala/akka/cluster/sharding/ShardCoordinator.scala>
- Cluster Singleton — <https://doc.akka.io/libraries/akka-core/current/typed/cluster-singleton.html>
- Cluster Bootstrap details — <https://doc.akka.io/libraries/akka-management/current/bootstrap/details.html>
- Discovery overview — <https://doc.akka.io/libraries/akka-core/current/discovery/index.html>
- Split Brain Resolver — <https://doc.akka.io/libraries/akka-core/current/split-brain-resolver.html>
- Phi-accrual failure detector — <https://doc.akka.io/libraries/akka-core/current/typed/failure-detector.html>

### Riak Core
- Introducing Riak Core — <https://riak.com/posts/business/introducing-riak-core/>
- Understanding handoff — <https://riak.com/posts/technical/understanding-riak_core-handoff/index.html>
- Riak Core tutorial — <https://github.com/lambdaclass/riak_core_tutorial>

### Failure detection / membership
- Phi-accrual paper (Hayashibara et al.) — <https://www.researchgate.net/publication/29682135_The_ph_accrual_failure_detector>
- SWIM paper — <https://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf>
- Lifeguard paper — <https://ar5iv.labs.arxiv.org/html/1707.00788>
- HyParView paper — <https://asc.di.fct.unl.pt/~jleitao/pdf/dsn07-leitao.pdf>
- HashiCorp memberlist — <https://github.com/hashicorp/memberlist>

### Transport
- QUIC overview — <https://www.chromium.org/quic/>
- TCP+TLS vs QUIC — <https://arxiv.org/pdf/1906.07415>
- `quinn` — <https://github.com/quinn-rs/quinn>
- HoL blocking in QUIC and HTTP/3 — <https://calendar.perfplanet.com/2020/head-of-line-blocking-in-quic-and-http-3-the-details/>

### Sharding / placement
- Consistent hashing — <https://en.wikipedia.org/wiki/Consistent_hashing>
- Rendezvous hashing — <https://en.wikipedia.org/wiki/Rendezvous_hashing>
- Damian Gryski on hashing tradeoffs — <https://dgryski.medium.com/consistent-hashing-algorithmic-tradeoffs-ef6b8e2fcae8>
- Jump consistent hash — <https://arxiv.org/pdf/1406.2294>

## Consistency: CRDT + consensus

### CRDTs
- Shapiro et al. survey — <https://inria.hal.science/inria-00555588v1/document>
- Almeida et al., Delta CRDTs — <https://arxiv.org/abs/1603.01529>
- Almeida et al., DVV — <https://gsd.di.uminho.pt/members/vff/dotted-version-vectors-2012.pdf>
- Matthew Weidner, CRDT Survey Part 3 — <https://mattweidner.com/2023/09/26/crdt-survey-3.html>
- Automerge — <https://github.com/automerge/automerge>
- Yjs — <https://github.com/yjs/yjs>
- Riak Datatypes — <https://docs.riak.com/riak/kv/latest/developing/data-types/>
- AntidoteDB — <https://github.com/AntidoteDB/antidote>
- Akka Distributed Data — <https://doc.akka.io/libraries/akka-core/current/typed/distributed-data.html>

### Causality
- Hybrid Logical Clocks — <https://martinfowler.com/articles/patterns-of-distributed-systems/hybrid-clock.html>
- CockroachDB joint consensus — <https://www.cockroachlabs.com/blog/joint-consensus-raft/>

### Consensus
- Raft paper / homepage — <https://raft.github.io/>
- openraft — <https://github.com/databendlabs/openraft>
- tikv/raft-rs — <https://github.com/tikv/raft-rs>
- etcd raft library — <https://github.com/etcd-io/raft>
- VR Revisited — <http://pmg.csail.mit.edu/papers/vr-revisited.pdf>
- EPaxos — <http://efficient.github.io/epaxos/>
- Tempo (EuroSys 2021) — <https://software.imdea.org/~gotsman/papers/tempo-eurosys21.pdf>
- Apache Cassandra Accord (CEP-15) — <https://cwiki.apache.org/confluence/display/CASSANDRA/CEP-15>
- Embedded openraft case study (Danube) — <https://dev-state.com/posts/migrate_danube_etcd_to_raft/>

### Leases & fencing
- Kleppmann distributed locks — <https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>
- Chubby (Burrows, OSDI 2006) — Google research
- Spanner TrueTime — <https://docs.cloud.google.com/spanner/docs/true-time-external-consistency>

### Replicated actors
- Akka vs Orleans — <https://github.com/akka/akka-meta/blob/master/ComparisonWithOrleans.md>
- Orleans grain model — <https://learn.microsoft.com/en-us/dotnet/orleans/overview>
- Linearizable SMR of state-based CRDTs without logs — <https://arxiv.org/abs/1905.08733>

## Durable execution

### Temporal
- Workflow definition rules — <https://docs.temporal.io/workflow-definition>
- Workflows overview — <https://docs.temporal.io/workflows>
- Child workflows — <https://docs.temporal.io/child-workflows>
- Signals / queries / updates — <https://docs.temporal.io/handling-messages>
- Continue-as-new — <https://docs.temporal.io/workflow-execution/continue-as-new>
- Execution limits — <https://docs.temporal.io/workflow-execution/limits>
- Retry policy — <https://docs.temporal.io/encyclopedia/retry-policies>
- Saga compensating transactions — <https://temporal.io/blog/compensating-actions-part-of-a-complete-breakfast-with-sagas>
- Non-determinism guidance — <https://www.bitovi.com/blog/replay-testing-to-avoid-non-determinism-in-temporal-workflows>
- Anti-patterns — <https://temporal.io/blog/spooky-stories-chilling-temporal-anti-patterns-part-1>

### DBOS
- Why DBOS — <https://docs.dbos.dev/why-dbos>
- Architecture — <https://docs.dbos.dev/architecture>
- Workflow tutorial — <https://docs.dbos.dev/python/tutorials/workflow-tutorial>
- DBOS vs Temporal — <https://www.tiarebalbi.com/en/blog/dbos-vs-temporal-postgres-durable-execution>

### Cadence
- Workflows — <https://cadenceworkflow.io/docs/concepts/workflows>
- Event handling — <https://cadenceworkflow.io/docs/concepts/events>

### Inngest
- Steps — <https://www.inngest.com/docs/learn/inngest-steps>
- How functions execute — <https://www.inngest.com/docs/learn/how-functions-are-executed>
- SDK spec — <https://github.com/inngest/inngest/blob/main/docs/SDK_SPEC.md>

### Restate
- Key concepts — <https://docs.restate.dev/foundations/key-concepts>
- Engine first principles — <https://www.restate.dev/blog/building-a-modern-durable-execution-engine-from-first-principles>
- Awakeables (TS docs) — <https://docs.restate.dev/develop/ts/awakeables/>
- Durable sessions for AI — <https://docs.restate.dev/ai/patterns/sessions>

### Sagas
- Garcia-Molina & Salem 1987 — <https://www.cs.cornell.edu/andru/cs711/2002fa/reading/sagas.pdf>
- Orchestration vs choreography — <https://blog.bytebytego.com/p/saga-pattern-demystified-orchestration>

### Event sourcing
- Azure pattern doc — <https://learn.microsoft.com/en-us/azure/architecture/patterns/event-sourcing>
- eventually-rs — <https://github.com/get-eventually/eventually-rs>
- primait/event_sourcing.rs — <https://github.com/primait/event_sourcing.rs>

### Continuations
- F# computation expressions — <https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/computation-expressions>
- Freer monads (Kiselyov) — <https://okmij.org/ftp/Haskell/extensible/more.pdf>
- Resonate how-it-works — <https://docs.resonatehq.io/evaluate/how-it-works>

## Agentic runtime

### Anthropic / Claude
- Building agents with Claude Agent SDK — <https://www.anthropic.com/engineering/building-agents-with-the-claude-agent-sdk>
- Tool use overview — <https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview>
- Advanced tool use — <https://www.anthropic.com/engineering/advanced-tool-use>
- Writing effective tools — <https://www.anthropic.com/engineering/writing-tools-for-agents>
- Agent Skills — <https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills>
- Prompt caching — <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>
- Extended thinking — <https://platform.claude.com/docs/en/build-with-claude/extended-thinking>
- Constitutional AI — <https://www-cdn.anthropic.com/7512771452629584566b6303311496c262da1006/Anthropic_ConstitutionalAI_v2.pdf>
- MCP spec 2025-11-25 — <https://modelcontextprotocol.io/specification/2025-11-25>

### Frameworks
- LangGraph persistence — <https://docs.langchain.com/oss/python/langgraph/persistence>
- LangGraph supervisor vs swarm — <https://focused.io/lab/multi-agent-orchestration-in-langgraph-supervisor-vs-swarm-tradeoffs-and-architecture>
- AutoGen v0.4 — <https://devblogs.microsoft.com/autogen/autogen-reimagined-launching-autogen-0-4/>
- AutoGen paper (arXiv 2308.08155) — <https://arxiv.org/pdf/2308.08155>
- DSPy — <https://dspy.ai/>
- DSPy paper (arXiv 2310.03714) — <https://arxiv.org/pdf/2310.03714>
- GEPA tutorial — <https://dspy.ai/tutorials/gepa_ai_program/>
- OpenAI Agents SDK — <https://openai.github.io/openai-agents-python/>
- Pydantic AI — <https://ai.pydantic.dev/>
- CrewAI — <https://docs.crewai.com/en/concepts/agents>
- Inngest AgentKit — <https://agentkit.inngest.com/overview>
- Mastra 2026 guide — <https://www.generative.inc/mastra-ai-the-complete-guide-to-the-typescript-agent-framework-2026>
- LlamaIndex ReAct workflow — <https://developers.llamaindex.ai/python/examples/workflow/react_agent/>

### Memory
- Letta / Mem0 / MemGPT 2026 — <https://tokenmix.ai/blog/ai-agent-memory-mem0-vs-letta-vs-memgpt-2026>
- Zep / Graphiti paper (arXiv 2501.13956) — <https://arxiv.org/abs/2501.13956>
- State of AI Agent Memory 2026 (Mem0) — <https://mem0.ai/blog/state-of-ai-agent-memory-2026>
- Park et al. Generative Agents (arXiv 2304.03442) — <https://arxiv.org/abs/2304.03442>
- ACON context compression (arXiv 2510.00615) — <https://arxiv.org/pdf/2510.00615>
- ReSum (arXiv 2509.13313) — <https://arxiv.org/pdf/2509.13313>
- Hybrid BM25 + vector — <https://medium.com/@pbronck/better-rag-accuracy-with-hybrid-bm25-dense-vector-search-ea99d48cba93>

### Multi-agent
- 2026 taxonomy — <https://www.digitalapplied.com/blog/agent-architecture-patterns-taxonomy-2026>
- Multi-agent architecture guide (Openlayer) — <https://www.openlayer.com/blog/post/multi-agent-system-architecture-guide>
- Multi-agent debate (review) — <https://hungleai.substack.com/p/agree-or-disagree-a-review-of-multi>
- Sakana AI Scientist v2 (arXiv 2504.08066) — <https://arxiv.org/pdf/2504.08066>
- CodeCRDT (arXiv 2510.18893) — <https://arxiv.org/pdf/2510.18893>

### Policies / guardrails
- NeMo Guardrails — <https://aisecurityandsafety.org/en/tools/nemo-guardrails/>
- OPA + AI agents — <https://codilime.com/blog/why-use-open-policy-agent-for-your-ai-agents/>
- OWASP Top 10 Agentic 2026 — <https://www.giskard.ai/knowledge/owasp-top-10-for-agentic-application-2026>
- AWS Four Security Principles — <https://aws.amazon.com/blogs/security/four-security-principles-for-agentic-ai-systems/>
- Prompt Injection 2026 Defense — <https://tekninjas.com/blogs/cybersecurity-ai-agents-prompt-injection-2026/>

### Evals
- LangSmith Evaluation — <https://www.langchain.com/langsmith/evaluation>
- agentevals — <https://github.com/langchain-ai/agentevals>
- Golden sets pattern — <https://medium.com/@falvarezpinto/evaluation-first-ai-product-engineering-golden-sets-drift-monitoring-and-release-gates-for-llm-2c3bfb3f1e7b>

### Human-in-the-loop
- Strata HITL 2026 — <https://www.strata.io/blog/agentic-identity/practicing-the-human-in-the-loop/>
- Cloudflare Agents HITL — <https://developers.cloudflare.com/agents/concepts/human-in-the-loop/>

### Durable agents
- Maxim Fateev: durable + AI — <https://workos.com/blog/maxim-fateev-temporal-durable-execution-ai-agents>
- Temporal Replay 2026 — <https://temporal.io/blog/replay-2026-product-announcements>
- Build durable AI agents with Pydantic AI + Temporal — <https://temporal.io/blog/build-durable-ai-agents-pydantic-ai-and-temporal>
- Idempotent AI agents — <https://www.buildmvpfast.com/blog/idempotent-ai-agent-retry-safe-patterns-production-workflow-2026>

## Language / codebase DB
- Unison Code Mappings — <https://www.unison-lang.org/docs/the-big-idea/>
- BLAKE3 — <https://github.com/BLAKE3-team/BLAKE3>

## Observability / simulation
- OTel docs — <https://opentelemetry.io/docs/>
- OTel for LLMs — <https://openobserve.ai/blog/opentelemetry-for-llms/>
- OTel for AI agents — <https://zylos.ai/research/2026-02-28-opentelemetry-ai-agent-observability>
- FoundationDB sim testing — <https://apple.github.io/foundationdb/testing.html>
- TigerBeetle sim testing — <https://tigerbeetle.com/blog/2023-07-11-we-put-a-distributed-database-in-the-browser>

## Crab Scheme existing specs (for continuity)
- `docs/research/beam_runtime_spec.md` — actor runtime
- `docs/research/channels_spec.md` — channels
- `docs/research/r6rs_extensions_spec.md` — R6RS++ language extensions
- `docs/research/realworld_benchmarks_spec.md` — bench harness
