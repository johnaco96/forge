# Phase 7 — Longitudinal Repository Health

Phases 0–6 answer *did this patch pass?* Phase 7 answers a different question:

> Did the repository improve over time?

It measures, compares, and reports. It never changes what Forge does in
response — acting on a trend is Phase 8, and nothing in this phase touches
routing, team planning, or task generation.

```text
WorldModelSnapshot @ commit   ─┐
evaluations / metrics @ commit ─┤──▶ RepositoryHealthSnapshot (immutable)
run + failure history          ─┘              │
                                               ▼
                        RepositoryHealthDiff · HealthTrend
```

---

## Repository health snapshots

A `RepositoryHealthSnapshot` (`H-0012`) is immutable and bound to one exact Git
commit. It records dimensions, raw measurements, provenance, and the exact
world-model snapshot it was built from. Re-inserting an identical snapshot is a
no-op; inserting different content under the same id is refused.

## Exact commit binding, and the candidate/head correction

**Every measurement is a claim about one commit.** A snapshot for commit `C`
contains only evidence measured at `C`, or window evidence ending through `C`.

The subtlety that motivated a dedicated type: a run record stores
`base_commit`, but the run's evaluators executed against the workspace *after*
the agent's patch was applied. Attaching a benchmark result to `base_commit`
would credit every measurement to the commit before the one it describes.

`MeasuredRepositoryState` resolves this per evidence kind:

| Evidence | Measured commit | Why |
|---|---|---|
| Ordinary run, patch committed | `patches.head_commit` (`CandidateHead`) | Evaluators ran against base + patch, which *is* the head commit |
| Run that produced no change | `runs.base_commit` (`BaseUnchanged`) | Nothing was applied, so the evaluated workspace really was the base |
| Run with changes never committed | **excluded** | Real numbers, but no commit id names the state they describe |
| Run with no patch record | **excluded** | The evaluated state cannot be named |
| Competitive experiment participant | same rule per participant | Each candidate is its own repository state |
| Team node | `team_nodes.output_commit` (`TeamNodeOutput`) | |
| Integrated team result | `team_executions.final_commit` (`TeamFinal`) | |
| Imported / legacy evidence with no patch row | **excluded** | |

When the state cannot be established, the evidence is excluded **with a stated
reason**, surfaced by `forge health build`. An unattributable measurement is
worse than a missing one: it looks trustworthy and is not.

## World-model linkage

A health snapshot references the exact Phase 6 world model for its commit. If
none exists, the build fails and tells you to run `forge world build`; an
`Ancestor` or `Stale` snapshot is never substituted. A `Partial` world model
propagates into a `Partial` health snapshot.

## Dimensions

Eleven roadmap dimensions, each `Available`, `Partial`, or `Unavailable`.
**Missing is never zero.**

| Dimension | Source | Notes |
|---|---|---|
| Test reliability | required test evaluators over the window | infrastructure errors excluded |
| Complexity | complexity-evaluator metrics | never synthesized |
| Dependencies | world-model dependency facts | total plus per-relationship-kind |
| Build time | build/test/lint durations | fingerprinted by exact command |
| Performance | structured benchmark metrics | no stdout scraping |
| Memory | structured metrics in byte units | stated convention, not inference |
| Security | security-evaluator verdict + emitted counts | no invented severity score |
| Duplication | structured duplication metrics only | otherwise unavailable |
| API stability | world-model interface counts | counts only; regression needs a contract |
| Failures | run outcomes over the window | `Errored` runs excluded from the denominator |
| Agent regressions | — | needs paired before/after; produced at diff time |

## Raw measurements

Values are stored in their original units with their own `Direction`
(`maximize` / `minimize` / `neutral`). There is no overall health score, and
normalized dimensions never replace the raw numbers.

## Point-in-time versus window

- **Point-in-time** — dependency count, interface count, build duration,
  benchmark values. Taken only from evidence measured at exactly the target
  commit.
- **Window** — test pass rate, failure rate. Bounded by *ancestry*: evidence
  measured at the target commit or an ancestor of it. Descendant evidence is
  never consumed, so a snapshot cannot be contaminated by a future its commit
  had not reached.

Every window measurement carries its denominator. A rate without one is
rejected by snapshot validation.

## Comparability identity

Two numbers with the same name are not a time series. `MeasurementIdentity`
keys a series by metric, unit, direction, source, **producer fingerprint**, and
component. The fingerprint is derived from the evaluator's exact command, so
`cargo test --lib` and `cargo test --workspace` never merge.

## Missing data

- Present only in the later snapshot → `NewlyAvailable`, never an infinite
  improvement.
- Present only in the earlier snapshot → `NoLongerAvailable`. A security scan
  that stopped running is not a repository with no findings.

## Materiality

Optional per-metric percentage thresholds, with an optional default. With no
threshold configured, a change is reported at its true magnitude and simply not
marked material — nothing is claimed either way.

## Attribution

| Level | Requires |
|---|---|
| `Confirmed` | produced the compared commit **and** a paired measurement moved beyond its declared threshold |
| `Supported` | produced the compared commit **and** a paired before/after measurement moved |
| `Associated` | produced the compared commit; nothing more |
| `Unknown` | no execution is known to have produced the commit |

Temporal proximity never raises the level. A commit with no recorded Forge
execution gets an **empty** attribution list — human and external-automation
commits are ordinary, and the health record stays valid for them.

## Diff semantics

Present in both and directional → the delta's sign against that direction.
Present in both, non-directional → a neutral change. `relation` records Git
ancestry; a diverged pair is reported as a structural comparison and explicitly
not as a chronology.

## Trend algorithm — `longitudinal-trend-v1`

For each comparable series, oldest first: take the first and last values,
compute percentage change, and compare its magnitude against the metric's
materiality threshold (or a 1% floor).

- below the threshold → `Stable`
- above, with a declared direction → `Improving` / `Degrading`
- above, without a declared direction → `Changing`
- fewer than 3 points → `InsufficientData`

This is a net-change rule, not a regression fit: a series that rises and falls
back reads `Stable`. Every trend stores its points, so the shape stays
inspectable.

### `Changing` and structural movement

Dependency and interface counts move without being better or worse.
`Changing` keeps that visible without a verdict. It never drives a reading
toward improving or degrading — but a dimension whose series are all
`Changing` reports `Changing`, not `Stable`, because a real change must not
hide behind a word meaning nothing happened.

## Overall status

Derived transparently from per-dimension directions. Disagreement is reported
as `Mixed`, which is more useful than a number that averages an improvement and
a regression into nothing. Dimensions with insufficient data do not vote as
`Stable`.

```text
Tests          Stable
Build time     Degrading
Performance    Improving
Dependencies   Changing

Overall        Mixed
```

## Ancestry and baseline selection

`forge health diff` defaults to the **nearest prior health snapshot on the same
ancestry chain** — not the latest row by timestamp. Snapshots on diverged
branches are excluded from automatic baseline selection; compare them by
passing two ids explicitly, and the output labels the result as structural.

## Versioning and reproducibility

- `health-v1` — persisted schema
- `health-builder-v1` — measurement collection semantics
- `longitudinal-trend-v1` — diff and trend interpretation

All three are persisted. Changing how a dimension is measured, or how a trend
is decided, must change the corresponding version so history is never silently
reinterpreted.

Each snapshot records its commit, its exact world model, the builder version,
the runs considered, and evidence references by existing id.

## CLI

```bash
forge health build          # immutable snapshot for the current exact commit
forge health show [H-0012]  # raw measurements, scopes, provenance
forge health diff [FROM] [TO]
forge health trend
```

`build` requires a clean working tree (health describes a commit, and a dirty
tree is not one) and an exact world model. It exits `0` when complete, `2` when
partial. A failed build never replaces the current-health pointer.

## Persistence

Migration `0011` is additive: `repository_health_snapshots`,
`repository_health_current`, `repository_health_dimensions`,
`repository_health_measurements`, `repository_health_evidence`,
`repository_health_events`. Canonical typed JSON with flat indexed columns,
following the Phase 6 pattern.

**Diffs and trends are not persisted.** They are deterministic functions of the
snapshots plus a recorded algorithm version; storing them would create a
derivable answer that could drift from its evidence. Run evidence, evaluator
results, and world-model facts are referenced by id, never copied.

## Limitations

- Trends use net change between endpoints, not a fit; oscillation reads as
  `Stable`.
- Component-level health is modeled (`MeasurementIdentity::component`) but no
  current extractor emits component-scoped metrics, so it is unexercised.
- Internal versus external dependencies are not split: the Phase 6 dependency
  fact records a relationship kind, not that distinction.
- Flaky-test detection is deliberately absent. Nothing is called flaky without
  repeated alternating evidence under comparable conditions.
- Agent-created regressions are not populated as a dimension; the attribution
  model produces them at diff time.
- Team and experiment evidence has typed measured-state rules but the builder
  currently collects from ordinary run evidence only.
- Invariant and contract regressions are not yet surfaced as trends.
- Analysis follows single ancestor chains; diverged branches are labelled, not
  merged into one history.

## Phase 7 evidence in Phase 8

Phase 7 continues to measure, compare, attribute conservatively, and report;
its truth semantics are unchanged. Phase 8 may admit comparable, cutoff-safe
health snapshots as policy evidence, but missing and partial data remain missing
or partial and a policy cannot change measurement direction. See
[`policy-optimization.md`](policy-optimization.md) for that consumer boundary.
