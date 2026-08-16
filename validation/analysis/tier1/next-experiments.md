# Tier 1 follow-on experiment readiness

Campaign: `forge-v1-2026-08`<br>
Master export SHA-256: `b283ef15c92f3c4c54f104900234638c2c46b2919a2f13a14f7435f3b27903b9`<br>
Generated: `2026-08-16T07:27:51.709350Z`

No experiment described here was executed.

## Context A/B

Frozen subset: `T-VAL-004`, `T-VAL-010`, `T-VAL-012`, `T-VAL-014`. A full two-arm run with one fixed agent/configuration is **8 additional runs**. Tier 1 produced 7/8 trustworthy PASSes on this subset, so PASS has limited headroom; runtime, tokens, patch behavior, and supplied fact counts are likely the more informative endpoints.

The preregistered rationale remains sound: T-VAL-004 depends on evaluator-contract knowledge beyond the edited file; T-VAL-010 spans a shared command preamble; T-VAL-012 depends on the policy resolver/persistence boundary; and T-VAL-014 depends on understanding what Phase 0–7 history the ledger actually contains. These are repository-context tasks selected before outcomes were known.

## Team vs single

| Task | Strongest/representative single | Claude | Codex |
| --- | --- | --- | --- |
| T-VAL-009 | claude/codex | PASS | PASS |
| T-VAL-010 | claude | PASS | PASS |
| T-VAL-012 | codex | INCONCLUSIVE | PASS |
| T-VAL-014 | codex | PASS | PASS |
| T-VAL-016 | codex | PASS | PASS |

The strongest observed single-agent baseline passed all five tasks. Team runs are ready only as a test of benchmark/resource improvement against that ceiling, at **5 additional team executions**.

## Longitudinal health

The dogfood-driven validation hardening was accepted into the v1.0.1 baseline, but no baseline health snapshot was built. No Tier 1 candidate change was accepted into `main`, and no three-snapshot comparable sequence exists. Longitudinal validation remains pending; isolated candidate branches cannot support a trend.

## Phase 8 policy optimization

Tier 1 contains 12 runs tagged `policy-evidence`, but **0 policy-controlled control/candidate observations**. All 40 formal runs predate policy control and are ineligible for a per-arm policy comparison. The default objective needs 8 comparable observations per arm and 3 health snapshots. A proposal now would be `InsufficientEvidence`; `HealthObservationPending` becomes the expected conservative state only after short-term arm evidence exists without the required health window. No proposal or promotion was executed.

## Tier 2

Recommendation: **RUN A REDUCED TIER 2** on `T-VAL-016` only (6 runs). Repeat only the preregistered performance task whose 10.2% benchmark separation could plausibly move under run-to-run noise. Do not repeat T-VAL-021 unchanged because its protected-path collision is a design issue, and the other preregistered tasks were paired PASSes without a conclusion-changing ambiguity.

## DeepSeek extension

Register, but do not mix, a separate cohort of 20 DeepSeek runs over the same frozen corpus and `v1.0.1` baseline after verifying a DeepSeek adapter/config and the evaluator layout. Give every task its own `independent-clone-v1` execution, preserve a new environment/ledger/export stratum, and analyze it as an extension rather than adding rows to the original Claude-vs-Codex Tier 1 estimate. It would test provider neutrality, evaluator portability, cold-start abstention, evidence accumulation for a new agent, and hidden Claude/Codex assumptions.
