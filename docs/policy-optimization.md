# Phase 8 — Engineering Policy Optimization

Phase 8 makes Forge's execution choices explicit, immutable, measurable, and
revisable. Its central boundary is:

> Forge optimizes its engineering policy, not the truth criteria used to judge Forge.

An engineering policy may tune bounded routing parameters, world-model context,
single/team strategy, advisory review, execution budgets, and exploration. It
cannot disable required evaluators, redefine PASS, weaken integrity or protected
paths, relabel provenance, change evidence eligibility, or alter repository and
user constraints. Those are structural `FixedGuardrail`s, not optimization
fields.

## Durable model

`EngineeringPolicy` is a repository-scoped immutable behavior record identified
by `P-*`. Its fingerprint covers every behavioral setting, objective, guardrail,
and lineage field; lifecycle status is stored separately because moving from
draft to canary to active does not change what the policy does. A repository has
one mutable `policy_current` pointer. Rollback re-points that pointer to an older
immutable policy rather than editing or deleting either policy.

Migration `0012_engineering_policy.sql` is additive. It stores policies,
proposals (`PP-*`), decisions (`PD-*`), shadow decisions, experiments (`PX-*`),
assignments, observations, and typed events. Three nullable columns link new
runs to the policy fingerprint and decision that governed them. Phase 0–7 runs
remain null and are never rewritten to claim policy control.

On the first Phase 8 command or run, Forge creates a bootstrap policy from the
repository's actual configuration. It preserves the Phase 7 configured-default
routing behavior, exact world-context configuration, single-agent execution,
team limits, no advisory review or retries, timeout limits, and exploration
settings. The record carries explicit `bootstrap` provenance.

## Evidence and reproducibility

The store returns run records; a dedicated `PolicyEvidenceResolver` decides
admissibility. Every considered run is either an eligible `PolicyObservation`
or an `ExcludedObservation` with a typed reason. Reasons include wrong
repository, outside the observation window, post-cutoff, collection limit,
manual override, disallowed provenance, infrastructure failure, missing policy
identity, policy mismatch, missing measured commit, and incomparable
configuration. Production optimization admits only explicitly live execution
provenance. Synthetic provenance must be opted into by deterministic tests.

A proposal has a fixed UTC cutoff. Runs, experiment observations, and health
snapshots recorded after it are excluded from that historical evidence set.
The evidence fingerprint covers admitted observation values and provenance,
excluded IDs and reasons, health references, policy fingerprints, configuration,
cutoff, and schema version. Re-resolving the same cutoff after later evidence
arrives reproduces the same snapshot; a later proposal gets a new cutoff and may
reach a different conclusion.

Health assembly remains outside `BaselineOptimizer`. It uses complete,
repository-scoped health snapshots recorded before the cutoff and matches them
to exact measured commits from eligible executions. Exact commit matching is a
stronger reproducible condition than inferring from the current `HEAD` and is
therefore ancestry-safe. Measurements must have the objective's direction and
the same producer/comparability identity on both arms. Missing, partial,
unavailable, differently directed, or incomparable measurements do not become
zero or success. A required longitudinal objective without enough observations
produces `HealthObservationPending`.

The deterministic `policy-baseline-v1` optimizer is pure:

```text
store/query layer
    → cutoff-safe PolicyEvidenceSnapshot
    → separately assembled HealthEvidenceValues
    → BaselineOptimizer
    → immutable PolicyProposal
```

It checks hard constraints before soft objectives, applies materiality
symmetrically, treats equality as neutral, and reports Pareto dominance or a
tradeoff rather than manufacturing one opaque score. A cold candidate is an
evidence state and normally calls for a controlled experiment, not a claim of
success.

## Execution decisions

The ordinary runner resolves, in order:

1. an explicit user override;
2. a persisted deterministic canary assignment;
3. the active policy.

It then applies the existing Phase 4 router and Phase 6 context selector. The
context record contains the exact commit-bound world snapshot and fact IDs.
Timeouts are capped by both the selected policy and repository configuration.
Before a run is linked, Forge persists a `PolicyDecision` naming the active and
selected policies, fingerprint, task revision, selection source, experiment arm,
context, and explanation. Store validation refuses contradictory active,
manual, or canary attribution.

An explicit command such as `forge run task.yaml --agent codex` always wins. The
decision records the active policy for context, the actual user choice, and
`manual_override`; the resolver excludes it from clean policy-controlled
evidence. Competition and explicit team-node runs are likewise distinguished
as manual policy overrides.

Phase 8 does not invent a team plan. The existing `forge team` path remains the
only team executor and requires a validated task DAG. An ordinary run refuses a
policy-selected `Team` strategy and directs the caller to that path. The current
bounded proposal CLI does not generate team or review changes; advisory review,
retry execution, and automatic team dispatch are therefore modeled but not
claimed as active Phase 8 behavior.

## Shadow and controlled experiments

A shadow policy records what it would select beside what the active policy did.
`policy_shadow_decisions` deliberately has no outcome column: the shadow choice
did not execute, cannot be credited with PASS, and is never candidate outcome
evidence.

A canary experiment persists its control/candidate policies, assignment rule
and version, proposal, budget, lifecycle, task-revision assignments, and actual
run observations. Assignment hashes the experiment ID, immutable task revision,
and version, so the same inputs always select the same arm. The stored
assignment cannot be flipped, and an observation must agree with its assignment,
policy linkage, and decision source.

`max_tasks` and expiration are enforced before accepting a new assignment.
Canary selection replaces the ordinary run, so it creates zero extra
executions; `max_extra_runs` remains a bound for workflows that actually add
executions. A known-cost ceiling never treats unknown cost as zero. Forge does
not know a coding-agent run's cost before it starts, so Phase 8 v1 reports that
pre-execution cost ceilings cannot be enforced rather than claiming otherwise.

## Promotion, approval, and rollback

Promotion is never implicit. `forge policy promote PP-*` is the human approval
action and requires a current, direct-successor candidate with matching
fingerprint and repository, an immutable proposal recommending promotion, a
concluded control/candidate experiment, sufficient evidence, completed required
health, satisfied hard constraints, valid bounds, unchanged objective, and all
fixed guardrails. The status changes, current pointer, and promotion event are
one transaction. Phase 8 v1 requires this explicit command even when a bounded
parameter change is classified `AutomaticAllowed`; it does not claim unattended
promotion.

Rollback is also explicit:

```bash
forge policy rollback P-0001 --reason "security health regressed"
```

The target must be a prior usable policy in the same repository and the reason
must be non-empty. Pointer, lifecycle statuses, and rollback event move in one
transaction. Runs, proposals, experiments, decisions, and the policy being left
remain queryable and unchanged.

## CLI

```bash
forge policy show
forge policy history
forge policy propose --max-world-facts 8
forge policy propose --timeout-secs 900
forge policy compare PP-0001
forge policy experiment create PP-0001 --candidate-share-percent 50 --max-tasks 20
forge policy experiment show PX-0001
forge policy experiment status PX-0001 execution-complete
forge policy experiment status PX-0001 concluded
forge policy promote PP-0002 --actor operator
forge policy rollback P-0001 --reason "observed regression" --actor operator
```

`propose` accepts only an existing candidate or bounded changes to world facts,
timeout, routing margin, or learned-routing use. It always resolves real
store evidence at a fixed cutoff and persists both the proposal and complete
evidence snapshot. `compare` prints arm outcomes, missing measurements,
runtime/tokens/cost, health references, exclusions, hard constraints, objective
results, and the tradeoff conclusion.

## Current boundary

Phase 8 is deterministic and operator-driven. It has no LLM optimizer, neural
or reinforcement learner, background daemon, autonomous task creation,
automatic remediation, web UI, or unattended promotion. It does not estimate
counterfactual shadow outcomes or pre-execution agent cost. Those absences are
intentional; no Phase 9 behavior is introduced here.
