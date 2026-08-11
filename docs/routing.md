# Routing contract and evidence policy

Phase 4A defines the trusted, provider-agnostic boundary that a future router
will consume. It does not select or execute an agent.

> Forge does not yet automatically select an agent in Phase 4A.

`forge run task.yaml --agent auto` exits with an explicit not-implemented
message. There is no partial routing or silent fallback.

## Immutable routing input

A `RoutingRequest` owns the exact `TaskRevision` being routed, a resolved
`CandidateAgentSet`, the evidence and exploration policies, readiness
thresholds, and a historical cutoff timestamp. The revision is a complete
content-addressed task definition; later edits to the same logical `TaskId`
cannot change the request or the historical observations returned for it.

`RoutingFeatures` contains only facts known before execution:

- task and revision identity;
- repository;
- declared category, language, domain, and difficulty;
- declared components and tags;
- deterministic lowercase objective terms.

Actual patch size, runtime, tokens, cost, evaluator output, and benchmark
improvement are not request features. Expected patch lines and code complexity
are explicitly marked unavailable because Forge cannot know them reliably
before execution. Forge does not invoke an LLM classifier or fabricate an
estimate.

## Execution provenance

Every new run records a typed `ExecutionProvenance`:

| Value | Meaning | Default production evidence |
|---|---|---|
| `live` | Genuine engineering execution | eligible for policy checks |
| `synthetic` | Deterministic fake/stub infrastructure execution | excluded |
| `imported` | Evidence supplied from outside this ledger | excluded |
| `unknown` | Provenance cannot be established | excluded |

Forge never infers provenance from an agent name, harness, model, or executable
path. Normal CLI executions assert `live`. Local CLI stub configurations used
by automated tests explicitly set `execution_provenance = "synthetic"`.
Programmatic runner requests default to `unknown` until their caller makes an
explicit assertion.

Migration 0007 assigns `unknown` to every older row. Some older runs may have
been genuine, but relabeling them `live` would be an unsupported trust claim.
Phase 0 through Phase 3 databases migrate in place; task-revision bindings and
ordinary ledger queries are unchanged.

## Default evidence policy

The versioned `routing-evidence-v1` policy considers runs for the requested
candidate identities up to the request cutoff. A run is eligible only when:

- its provenance is `live`;
- the Forge pipeline reached `Completed` rather than failing or being
  cancelled;
- an agent execution record exists and did not fail to start or get cancelled;
- Forge recorded a non-`ERROR` outcome;
- integrity is present and clean;
- comparable immutable task metadata reaches the configured similarity floor;
- changed-work outcomes have evaluation evidence;
- evaluators had no infrastructure execution errors.

Agent nonzero exits and timeouts are not automatically infrastructure failures.
If Forge still captured and evaluated a patch, the trusted Forge outcome remains
the target. By contrast, `RunStatus::Failed`, a start failure, or `ERROR` means
Forge could not establish an engineering result and is excluded.

Every in-scope row appears either as an eligible record or as one typed
exclusion. The summary reports totals and counts such as synthetic, unknown,
integrity violation, incomplete/infrastructure failure, missing evaluation,
and evaluator infrastructure failure. Nothing is silently discarded.

Synthetic rows remain visible in `forge history`, agent statistics, failures,
experiments, similarity, and JSONL export. Only production routing evidence
excludes them by default.

## Outcome targets

The first router's target contract is derived only from Forge's trusted
`RunOutcome`:

| Forge outcome | Routing target |
|---|---|
| `PASS` | positive |
| `FAIL` | negative |
| `INCONCLUSIVE` | unresolved/inconclusive |
| `NO_CHANGE` | unresolved/no-change |
| `ERROR` | infrastructure exclusion |

`FAIL` is a normal negative engineering observation. `INCONCLUSIVE` and
`NO_CHANGE` remain distinct and visible; neither is coerced to success or
failure. Integrity violations are excluded by the default policy, and a
malformed positive row with unacceptable integrity can never be admitted as
positive evidence even under a relaxed integrity filter. No weighted quality
score is introduced.

## Evidence record and readiness

`RoutingEvidenceRecord` is compact and provider-agnostic. It contains the run
and task-revision IDs, exact historical pre-run features, agent configuration
and fingerprint, similarity evidence, lifecycle/process/outcome facts,
integrity, evaluator summary, agent runtime, provider-reported usage, known
cost, provenance, experiment membership, and creation time. It never embeds
logs, prompts, diffs, patch content, or raw SQLite rows.

Readiness thresholds are intentionally small and configurable:

```toml
[routing]
minimum_total_evidence = 10
minimum_agent_evidence = 3
exploration_policy = "compete_when_uncertain"
```

Only resolved positive/negative observations satisfy the minima. Trustworthy
unresolved records remain eligible and explainable but cannot manufacture a
predictive sample size. `RoutingReadiness::InsufficientEvidence` returns typed
reasons, including no eligible live history, no comparable revisions, only one
candidate with resolved evidence, insufficient total evidence, and a candidate
below its per-agent minimum. Forge does not invent probabilities or force a
recommendation.

Candidate resolution is provider-agnostic. A candidate must be registered,
implemented, explicitly available/configured, and satisfy required
capabilities. The contract stores exact configuration fingerprints and does
not hard-code Claude or Codex, allowing future local and specialized agents.

## Exploration and decision contracts

The exploration policy supports `none`, `compete_when_uncertain`, and
`periodic_competition`. These are contracts only; Phase 4A does not launch a
competition. A future insufficient-evidence decision can suggest gathering
live evidence, manual selection, or a comparative run without pretending an
agent is already known to be best.

`RoutingDecision` has typed `Selected`, `InsufficientEvidence`, and
`CompeteRecommended` forms. Explanations contain structured evidence counts,
similar-task counts, per-agent observations, exclusion counts, readiness
reasons, a decision source, and a policy version. Phase 4A does not construct a
`Selected` decision and contains no heuristic or learned selection algorithm.

## Reproducibility

Every query returns a `RoutingEvidenceSnapshot` containing:

- routing-contract and evidence-policy versions;
- an explicit absent routing-policy version in Phase 4A;
- exact target task revision;
- sorted candidate configuration fingerprints;
- historical cutoff timestamp;
- minimum-evidence configuration;
- deterministically ordered eligible run IDs;
- a SHA-256 fingerprint over the complete request, eligible records, and
  typed exclusions.

The fingerprint changes if the evidence or policy input changes and is stable
for repeated queries over identical state. It prevents a later query against a
changed ledger from being represented as the evidence behind an earlier
decision. This is not a model registry.

## Phase boundary

Phase 4A does not implement logistic regression, boosted trees, LLM routing,
configuration optimization, automatic competition, multi-agent execution,
task decomposition, scheduling, repository world models, or automatic agent
execution. The real routing algorithm and activation of `--agent auto` require
a separate architectural review.
