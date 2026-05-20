# Security — capabilities, mTLS, effect permissions, audit

Crate this spec creates: **`cs-cap`**. Milestone **M10**. Task list:
`tasks/M10-capabilities-and-policy.md`.

## Principles

From OWASP Top 10 for Agentic Applications 2026 and AWS's "Four
Security Principles for Agentic AI Systems":

1. **Deny by default.** No tool call, no network egress, no file
   read happens without an explicit policy decision.
2. **Least privilege.** Each agent / workflow / actor gets a
   capability set that's the minimum it needs.
3. **Capabilities flow through the call graph.** Child actors
   inherit parent's caps; explicit attenuation drops privileges.
4. **The runtime, not the model, decides what's allowed.** Even a
   successful prompt injection is contained: the tool call must
   pass policy before reaching the upstream service.

References:
- OWASP Top 10 Agentic 2026 — <https://www.giskard.ai/knowledge/owasp-top-10-for-agentic-application-2026>
- AWS Four Security Principles — <https://aws.amazon.com/blogs/security/four-security-principles-for-agentic-ai-systems/>
- Object-Capability Model — <http://erights.org/elib/capability/index.html>

## The `Capability` primitive

A capability is an unforgeable token granting one specific
permission. The runtime mints them; user code can attenuate, pass,
and revoke, but not forge.

```scheme
(define cap-read-file (capability 'fs/read #:path "/workspace"))
(define cap-net       (capability 'net/connect #:host "api.openai.com"))

(with-capabilities (list cap-read-file)
  (define data (read-file "/workspace/data.json")))

;; Attenuated cap: child can read only ./subdir
(define child-cap
  (cap-attenuate cap-read-file #:path "/workspace/subdir"))
```

Implementation: every privileged primitive (`open-file`,
`http-connect`, `(tool-call …)`, `(spawn-remote …)`, `(lease-acquire! …)`)
consults a thread-local capability set. The capability flows
through call chains via dynamic extent (parameterize-like). Spawning
an actor snapshots the parent's caps; the child cannot acquire new
ones, only attenuate.

## Policy DSL

Higher-level: a policy is a predicate over `(action, context)` that
returns `'allow`, `'deny`, or `'require-approval`.

```scheme
(define-policy production-safety
  ;; Predicate-style:
  #:on tool-call
  (lambda (call ctx)
    (cond
      ((not (production? ctx)) 'allow)
      ((in (call-tool call) '(delete-db restart-cluster))
       (if (human-approved? call) 'allow 'require-approval))
      (else 'allow))))

(define-policy egress-allowlist
  #:on net/connect
  (lambda (req ctx)
    (if (member (req-host req) approved-hosts)
        'allow 'deny)))

(define-agent prod-agent
  #:tools (list shell read-file)
  #:policies (list production-safety egress-allowlist))
```

Policies compose. The runtime evaluates them in declaration order;
the first non-`allow` decision wins.

## Guardrails (input/output)

Distinct from policies (which gate *actions*); guardrails gate
*data*. Input guardrails screen user messages for prompt injection;
output guardrails redact PII / credentials before they leave the
agent.

```scheme
(define-guardrail prompt-injection-detector
  #:on 'message-in
  (lambda (msg)
    (if (prompt-injection? msg)
        (deny "potential prompt injection")
        (allow))))

(define-guardrail pii-redactor
  #:on 'message-out
  (lambda (msg)
    (redact-pii msg)))      ; mutates → returns sanitized msg

(define-agent customer-agent
  ...
  #:input-guardrails (list prompt-injection-detector)
  #:output-guardrails (list pii-redactor))
```

The runtime built-in `prompt-injection?` calls a small model
(`claude-haiku` by default) to score the input. Configurable.

Reference: NeMo Guardrails — <https://aisecurityandsafety.org/en/tools/nemo-guardrails/>

## mTLS

Cluster handshake (see `distributed.md`) uses rustls mTLS. Cluster
CA is configurable. Certificates carry node identity in CN; SAN
entries list external addresses; node enrollment is a separate
operational concern (out of scope for this spec — admin uses cert-manager,
HashiCorp Vault PKI, or hand-managed certs).

## Effect permissions

Effect annotations (M01) plus capabilities give a static-and-dynamic
permission model:

- Static (M01): `(define foo #:effects '(net audit))` — function
  declares it may do net + audit. Body is checked.
- Dynamic (M10): the function's actual capability set is enforced
  at call time. A `net` effect with no `net/connect` cap = runtime
  deny.

Together, this prevents both *unintended* effects (compile-time
catches them) and *malicious* effects (runtime denies).

## Audit log

Every policy decision and every privileged action is appended to
an audit log:

```
{
  ts: HLC,
  actor: pid,
  agent: name,
  action: tool-call | net-connect | file-open | ...,
  args-hash: blake3:...,
  policy-decision: allow | deny | require-approval,
  approver: pid | null,
  outcome: success | failure | denied,
  correlation-id: uuid
}
```

Backed by `cs-stdlib-wal` (M11 new crate). Append-only,
queryable, retention-policied. Optional OTel export.

## Code-hash allowlists

For supply-chain hardening, the cluster can pin allowed code hashes:

```scheme
(cluster
  ...
  #:code-allowlist
    '(production-set
       #blake3:abc123 ; namespace/foo:v1
       #blake3:def456 ; namespace/bar:v1
       ...))
```

`spawn-remote` of any function whose hash isn't in the allowlist
fails. Used in conjunction with cs-codebase (M12) for reproducible
deploys.

## v1 minimum

- `(capability …)` primitive + thread-local cap set.
- `(define-policy …)` + `(define-guardrail …)` forms.
- Policy gate on `(tool-call …)`, `(net/connect …)`, `(open-file …)`, `(spawn-remote …)`, `(lease-acquire! …)`.
- One built-in prompt-injection detector.
- Audit log (cs-stdlib-wal).
- mTLS handshake in cs-distrib (shared with M02).
- Defer: full OPA/Rego integration; capability-revocation primitives; code-allowlist.

## Code pointers

- `crates/cs-sandbox-wasm/` — existing L1+L2 sandbox; caps applied at L1 level.
- `crates/cs-runtime/src/builtins/mod.rs` — gate every privileged b_* primop with a cap check.
- `crates/cs-runtime/src/lib.rs` — `Runtime` already has `sandbox_import_policy`; extend with cap set.

## External references

- Object-Capability Model — <http://erights.org/elib/capability/index.html>
- OPA — <https://www.openpolicyagent.org/docs/policy-language>
- OWASP Top 10 Agentic 2026 — <https://www.giskard.ai/knowledge/owasp-top-10-for-agentic-application-2026>
- AWS Four Security Principles — <https://aws.amazon.com/blogs/security/four-security-principles-for-agentic-ai-systems/>
- NeMo Guardrails — <https://aisecurityandsafety.org/en/tools/nemo-guardrails/>
- Prompt Injection 2026 Defense — <https://tekninjas.com/blogs/cybersecurity-ai-agents-prompt-injection-2026/>
- Constitutional AI — <https://www-cdn.anthropic.com/7512771452629584566b6303311496c262da1006/Anthropic_ConstitutionalAI_v2.pdf>
