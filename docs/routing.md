# Historical baseline routing

Forge can select one currently available agent configuration with:

```bash
forge run task.yaml --agent auto
```

Phase 4B builds directly on the Phase 4A trust boundary. The router receives an
immutable `TaskRevision`, exact current candidate configuration fingerprints,
the versioned evidence policy, readiness thresholds, exploration policy, and a
historical cutoff. A selected adapter then enters the same ordinary Forge run
pipeline used by explicit agent invocations. Routing never creates a second
workspace, prompt, patch, evaluation, event, or artifact path.

## Evidence and identity

`routing-evidence-v1` admits genuine `live` completed runs with an execution
record, a Forge outcome, acceptable integrity, comparable immutable task
metadata, and usable evaluator evidence. `synthetic`, `unknown`, and `imported`
provenance are excluded by default. Imported evidence can be admitted only by
an explicit programmatic evidence policy. Infrastructure failures are excluded
rather than mislabeled as engineering failures.

Evidence is matched to the exact current `AgentConfig` fingerprint—not merely
the provider name. A historical run from another model, harness, permission
mode, executable setting, or other stable configuration is reported as a
configuration-mismatch exclusion. Forge selects only among current registered
configurations; it does not search arbitrary configurations.

PASS is positive and FAIL is negative. INCONCLUSIVE and NO_CHANGE remain
separate unresolved records: they appear in counts and explanations but do not
vote in the success estimate. Experiment membership and manual, automatic, or
competition selection source are retained for analysis. They receive no
special weight in this first policy.

## `historical-baseline-v1`

For each candidate, every resolved historical observation has weight equal to
its deterministic task-similarity score. Repository, classification, domain,
components, tags, and objective terms therefore influence routing through the
accepted similarity model; no embeddings or LLM are used.

With weighted positive evidence `P`, weighted negative evidence `N`, and
configured Beta prior `α, β`:

```text
predicted_success = routing_score = (P + α) / (P + N + α + β)
evidence_strength = (P + N) / (P + N + α + β)
```

The defaults `α = 1` and `β = 1` prevent a single PASS from becoming a 100%
prediction. Candidates are sorted by score, then agent ID for deterministic
ties. The five highest-similarity historical runs per candidate are persisted
as influential evidence. This is a transparent historical decision policy,
not a statistical-significance claim or universal agent ranking.

```toml
[routing]
minimum_total_evidence = 10
minimum_agent_evidence = 3
minimum_score_margin = 0.05
exploration_policy = "compete_when_uncertain"
periodic_competition_interval = 10

[routing.baseline]
prior_alpha = 1.0
prior_beta = 1.0
```

Only resolved observations satisfy readiness. Every candidate must meet the
per-agent minimum and the cohort must meet the total minimum. A winner is
selected only when the leading score exceeds the runner-up by at least the
configured margin.

Exactly one available candidate returns `InsufficientEvidence` with an
only-available-candidate reason and asks for explicit selection. Forge does not
describe availability as a learned preference. No available candidates is a
clear configuration error.

## Exploration outcomes

- `none`: ready, separated scores select; insufficient or close evidence stops
  with `InsufficientEvidence`. It never launches competition.
- `compete_when_uncertain`: insufficient or close evidence returns
  `CompeteRecommended`. The CLI stops and prints an explicit `forge compete`
  command; it does not launch hidden work.
- `periodic_competition`: uncertainty also recommends competition. Even with a
  clear leader, competition is recommended when the eligible resolved count is
  a nonzero multiple of `periodic_competition_interval`. A subsequent real
  competition normally advances the count, giving a small deterministic
  cadence without a bandit or scheduler.

Routing stops use process exit code 3. This is distinct from exit code 1
(Forge/configuration failure) and exit code 2 (an executed run did not pass).

## Persistence, explanation, and reproduction

Migration 0008 adds durable routing decision and routing-event records. Each
decision stores its ID, optional resulting run, task and revision, timestamp,
candidates and exact configurations, selected configuration, decision kind,
router/evidence-policy versions, policy parameters, cutoff, evidence
fingerprint, counts, typed exclusions, candidate scores, margin, influential
runs, readiness, and structured explanation. Lifecycle events are
`RoutingStarted`, `RoutingEvidenceResolved`, and one typed terminal event.

An auto-selected run separately records `SelectionSource::Automatic` with the
decision ID, router version, and evidence fingerprint. Manual and competition
runs retain their typed sources. This never changes `ExecutionProvenance`: a
manual and an automatic run can both be genuine `live` evidence.

The snapshot SHA-256 covers the complete request, eligible ordered evidence,
and typed exclusions. Together, task revision, exact candidates, router
version, policy configuration, cutoff, and fingerprint reproduce the same
decision. Runs added after the cutoff cannot rewrite a persisted decision.
Phase 0–4A databases migrate in place; older runs become manual selection and
keep their existing immutable task revisions and provenance.

## CLI behavior

A selected route prints candidate scores, counts, selected configuration,
margin, router version, decision ID, and fingerprint before the ordinary run
report, whose selection line reads `AUTO → <agent>`.

If history is below readiness, `none` prints `INSUFFICIENT EVIDENCE`; the
default uncertainty policy prints `COMPETITION RECOMMENDED`. Close scores do
the same under the configured margin. Neither case runs an agent.

## Limitations and feedback loops

Automatic genuine runs may become future eligible evidence. Their typed source
is retained so later policies can study selection bias, but v1 does not
reweight or exclude them. Competition evidence is identifiable but not
privileged. The policy has no confidence intervals, embeddings, configuration
optimization, bandit exploration, LLM routing, task decomposition, teamwork,
or self-modification. Changing decision-affecting behavior requires a new
router version.
