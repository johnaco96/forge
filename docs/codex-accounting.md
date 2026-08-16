# Codex accounting evidence

Forge keeps four accounting quantities separate because they do not have the
same provenance or meaning:

- `known_cost_usd` is monetary cost explicitly reported or directly billed by
  the provider for the run. A Codex run included in a ChatGPT subscription has
  `known_cost_usd = null` unless Codex actually reports a dollar amount.
- `provider_reported_credits` is credit consumption directly emitted by
  Codex/OpenAI tooling.
- `derived_credits` is Forge's deterministic calculation from raw token classes
  and a pinned official credit rate card.
- `credit_equivalent_usd` is an optional standardized USD-equivalent derived
  from credits using an explicit conversion basis. It is not billed cost.

The installed Codex CLI 0.147.0 `exec --json` stream was inspected against
preserved formal-campaign runs. Its `turn.completed.usage` object reports:

```text
input_tokens
cached_input_tokens
cache_write_input_tokens
output_tokens
reasoning_output_tokens
```

The final `input_tokens` includes cached input; cached input is a subset, not an
additional volume. In the matching provider session evidence, `total_tokens`
equals input plus output, so reasoning output is also not added a second time.
The captured `exec --json` stream does not itself report total tokens, the
model, credits, or dollar cost. Matching Codex rollout session logs do record
the provider total and actual model in `turn_context.payload.model`; the offline
record labels that rollout as a supplemental source instead of relabeling a
Forge sum as provider evidence. A rollout is accepted only when its session ID
matches the thread ID in the captured agent log.

Missing values remain absent. In particular, a missing cached-input field does
not mean zero cached tokens.

## Versioned rate card

`forge-accounting` contains the smallest checked rate card needed by the
current campaign:

```text
id       openai-chatgpt-codex-credits-2026-08-15
model    gpt-5.6-sol
input    125 credits / 1M tokens
cached   12.5 credits / 1M tokens
output   750 credits / 1M tokens
cache writes  NotCharged (0 credits)
```

The rates were retrieved on 2026-08-15 from the official
[ChatGPT and Codex pricing documentation](https://learn.chatgpt.com/docs/pricing).
They are checked into Forge; normal calculation performs no network request.
The current official Codex rate card does not charge for cache writes. Forge
preserves `cache_write_input_tokens` as raw provider evidence, while this dated
rate card records the billing policy as `NotCharged`. Cache-write billing is a
versioned rate-card property rather than a global accounting assumption.

For this Codex JSONL contract:

```text
uncached_input = input_tokens - cached_input_tokens

derived_credits =
    uncached_input_tokens / 1,000,000 × input_credit_rate
  + cached_input_tokens   / 1,000,000 × cached_credit_rate
  + output_tokens         / 1,000,000 × output_credit_rate
```

The official documentation available for this implementation did not establish
one stable credit-to-USD purchase conversion applicable to this account and
plan. `credit_equivalent_usd` and its conversion basis therefore remain absent.
API token pricing is not substituted: API cost and ChatGPT credit consumption
are different accounting bases.

## Offline enrichment during Tier 1

Runtime persistence is intentionally deferred while the formal Tier 1 campaign
is active. The frozen campaign continues to use `v1.0.1`; `forge run`, agent
invocation, prompts, timeout, sandbox, evaluation, outcomes, persistence, and
the schema-1 export are unchanged.

The separate `forge-accounting` binary reads already-preserved evidence and
writes a new JSONL artifact:

```bash
cargo run -p forge-accounting -- enrich-codex \
  --environment /archive/environment.json \
  --export /archive/T-VAL-001.codex.export.jsonl \
  --agent-log /archive/participants/T-VAL-001-codex/.forge/runs/R-0001/agent.stdout.log \
  --session-log "$CODEX_SESSION_LOG" \
  --output /separate-analysis/T-VAL-001.codex.accounting.jsonl
```

The output key includes campaign ID, task ID, immutable task revision, agent,
base commit, and ledger-local run ID. This prevents independent participant
ledgers that each contain `R-0001` from colliding. Every source artifact is
referenced by path and SHA-256 digest. The original export and ledger are never
opened for writing, and PASS/FAIL/INCONCLUSIVE is merely copied as the original
outcome for traceability.

If the session log is unavailable and the immutable Forge run configuration did
not explicitly pin a model, the model and `derived_credits` remain unknown.

Coverage is reported without combining incompatible bases:

```bash
cargo run -p forge-accounting -- coverage /separate-analysis/*.accounting.jsonl
```

The output separately counts model coverage, token coverage, provider credits,
derived credits, credit-equivalent USD, and known billed USD. It does not name a
cost winner between Claude provider-reported dollars and Codex credits.

## Limitations

`NotCharged` applies only to
`openai-chatgpt-codex-credits-2026-08-15`. A later rate card must state its own
cache-write billing policy. Forge blocks exact derivation when a future card's
policy is unknown and nonzero cache-write usage could affect the result; it does
not project the current policy onto historical or future cards.
