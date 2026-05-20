# M10 — Capabilities + policy DSL (cs-cap)

**Crates created:** `cs-cap`.
**Effort:** 3-4 iters.
**Depends on:** M01 (effects), M09 (tool primops to gate).

## Goal

`(define-policy …)`, `(define-guardrail …)`, `(capability …)`,
audit log. Deny-by-default; capabilities flow through call graph;
every privileged primop checks them.

## Acceptance

- A tool call with a deny policy raises `&policy-denied` before the handler runs.
- Child actor spawned without parent's caps cannot acquire them.
- Audit log captures every privileged action with structured fields.
- Built-in prompt-injection detector classifies a known-bad input correctly.
- mTLS handshake in cs-distrib uses caps for handshake authorization.

## Iters

### A — `capability` primitive + thread-local set

- `(capability KIND #:KEY VAL …)` constructs an unforgeable token.
- Thread-local set; `(with-capabilities (list c …) body)` scopes them.
- `(cap-attenuate cap #:KEY VAL)` returns a narrowed cap.
- **Code:** new `crates/cs-cap/src/lib.rs`.

### B — `define-policy` + `define-guardrail` forms

- Policy: predicate over `(action, ctx) → 'allow | 'deny | 'require-approval`.
- Guardrail: predicate over `(data, ctx) → 'allow | (redacted-data)`.
- Multiple policies/guardrails compose left-to-right; first non-allow wins.

### C — Gate every privileged primop

- `(tool-call …)`, `(net/connect …)`, `(open-file …)`, `(spawn-remote …)`, `(lease-acquire! …)`, `(replicated-actor-call! …)`.
- Each invokes the runtime's cap-check + policy-check.
- Returns `&policy-denied` condition (subcondition of `&error`).

### D — Built-in prompt-injection detector + audit log

- `(prompt-injection? msg)` calls a small model.
- Audit log writes via cs-stdlib-wal (new in M11 or here as a pre-req).

## Example

```scheme
(define-policy production-safety
  #:on 'tool-call
  (lambda (call ctx)
    (cond
      ((not (production? ctx)) 'allow)
      ((in (call-tool call) '(delete-db restart-cluster))
       (if (human-approved? call) 'allow 'require-approval))
      (else 'allow))))

(define-policy egress-allowlist
  #:on 'net/connect
  (lambda (req ctx)
    (if (member (req-host req) approved-hosts)
        'allow 'deny)))

(define-guardrail pii-redactor
  #:on 'message-out
  (lambda (msg) (redact-pii msg)))

(define-agent prod-agent
  #:tools (list shell read-file)
  #:policies (list production-safety egress-allowlist)
  #:output-guardrails (list pii-redactor))
```

## External refs

- Object-capability model — <http://erights.org/elib/capability/index.html>
- OPA — <https://www.openpolicyagent.org/docs/policy-language>
- OWASP Top 10 Agentic 2026 — <https://www.giskard.ai/knowledge/owasp-top-10-for-agentic-application-2026>
- AWS Four Security Principles — <https://aws.amazon.com/blogs/security/four-security-principles-for-agentic-ai-systems/>
- NeMo Guardrails — <https://aisecurityandsafety.org/en/tools/nemo-guardrails/>

## Code pointers

- `crates/cs-sandbox-wasm/` — existing L1+L2 sandbox (caps at L1).
- `crates/cs-runtime/src/builtins/mod.rs` — gate b_* primops.
- `crates/cs-runtime/src/lib.rs` — existing `sandbox_import_policy`; extend with cap set.
