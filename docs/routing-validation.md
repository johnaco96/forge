# Routing correctness and validation

## Production contract

The historical-baseline-v1 router optimizes one transparent primary objective:
estimated probability of a trustworthy PASS. Integrity and execution trust are
hard eligibility constraints. Runtime, provider usage/cost, benchmark metrics,
and patch size are reported but do not enter a hidden scalar score.

For production V1, keep success probability primary. A future lexicographic
policy may prefer runtime/cost only among candidates proven sufficiently
equivalent on success and safety. That would require a separately versioned,
reviewed policy.

## Temporal and snapshot semantics

A run created before cutoff is not automatically evidence. Its terminal
finished_at must be at or before cutoff. When an evaluation exists, its own
finished_at must also be at or before cutoff. Missing completion timestamps
fail closed. Synthetic, imported, unknown-provenance, infrastructure,
integrity, evaluator-infrastructure, low-similarity, task-revision, and
effective-config mismatches remain explicit exclusions.

Routing establishes one SQLite read transaction, then retrieves runs, immutable
task definitions, evaluations, and indexed identities in one joined statement.
A run committed after the snapshot starts cannot appear halfway through the
decision. The evidence fingerprint names the sorted eligible and excluded view.
Decision and lifecycle events commit atomically.

Effective configuration fingerprint version 2 includes agent/provider identity,
harness and recorded harness version, model, tools/settings, timeout, execution
policy fingerprint, and containment/resource/network configuration. Historical
version-1 fingerprints retain their original algorithm and unknown historical
facts stay unknown.

## Exact Tier 1 replay

The replay executable imports only evidence that existed before each historical
decision into a temporary SQLite store and invokes the production Store and
RoutingContract:

    cargo run -p forge-router --bin forge-router-replay -- \
      --input validation/fixtures/tier1-router-replay.jsonl --summary

Repeated summary output is byte-identical. Tier 1 contained 20 paired tasks; 14
were post-readiness, and the largest post-readiness score margin was about 0.021633.
At the configured threshold 0.05, zero selections is therefore the correct
router result, not a Python reimplementation error.

Offline threshold research is checked in at
validation/results/routing-tier1-calibration.json. Coverage at thresholds
0.05, 0.03, 0.02, 0.01, and 0.00 was respectively 0%, 0%, 10%, 15%, and 70%.
The artifact separates PASS rate, accuracy, and regret and explains that the
campaign winner sometimes used secondary dimensions the router does not
optimize. The production threshold remains 0.05.

## Prospective holdout

validation/routing-holdout/campaign.json preregisters 12 unseen, balanced task
slots. Every recommendation or abstention must occur and persist before paired
Claude/Codex execution. Threshold, router version, effective configs, and
immutable baselines freeze before outcomes. Abstention falls back to paired
ground truth, never to silent automatic selection.

Do not execute the holdout as part of hardening. Until its coverage, selected
PASS, correctness, abstention, regret, runtime, and usage effects are reviewed,
automatic routing is not validated for autonomous production.

## Operating modes

- manual: pass a concrete agent; humans remain authoritative.
- recommend: resolve and persist routing, print it, execute nobody.
- automatic: explicit --agent auto authorizes a selected agent, but is not the
  default and is not approved for unattended production.

When learned routing is active and no agent is supplied, Forge chooses
recommendation mode. Abstention exits without agent execution.
