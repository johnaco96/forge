# Validation task taxonomy

Forge deliberately keeps classification values as repository-defined strings
(`TaskClassification` is four optional `String` fields, validated only for
length and control characters). That flexibility is right for the core, and
wrong for a campaign: cohort statistics are only meaningful if every task in a
cohort used the same spelling.

This file is the campaign's controlled vocabulary. It is enforced by
`scripts/validate-corpus.sh`, not by `forge-core`.

---

## `classification.category`

| Value | Meaning |
|---|---|
| `debugging` | Correct an existing behaviour that is wrong, unsafe, or silently lossy |
| `feature` | Add a capability that does not exist |
| `refactor` | Change structure without changing behaviour |
| `testing` | Add or strengthen independent verification of an existing invariant |
| `performance` | Reduce time, allocations, or query count without changing results |
| `persistence` | Schema, migration, or durable-record work |

Six values, matching the roadmap's recommended distribution. `implementation`
(used by the `median` smoke fixture) is deliberately **not** in this vocabulary
— that fixture is infrastructure verification, not campaign evidence.

## `classification.difficulty`

| Value | Meaning |
|---|---|
| `small` | One file, one clear change, obvious done condition |
| `medium` | Several files or one non-obvious design decision |
| `hard` | Cross-crate, or requires understanding an invariant before changing anything |

Difficulty is the author's estimate of *engineering* difficulty, not token
count. It is recorded so results can be read by difficulty, and it is never
used to excuse a failure after the fact.

## `classification.language`

`rust` for the entire corpus. Recorded explicitly so a future polyglot campaign
can be separated from this one.

## `classification.domain`

| Value | Meaning |
|---|---|
| `core` | `forge-core` domain model |
| `persistence` | `forge-store`, migrations, ledger queries |
| `cli` | `forge-cli` surface and output |
| `policy` | `forge-policy`, Phase 8 optimization |
| `evaluation` | `forge-eval`, evaluators, metrics |
| `execution` | `forge-runner`, `forge-executor`, `forge-agent` |

## `components`

Crate names exactly as they appear in `crates/`: `forge-core`, `forge-store`,
`forge-cli`, `forge-policy`, `forge-eval`, `forge-runner`, `forge-agent`,
`forge-router`, `forge-health`, `forge-world`, `forge-team`, `forge-git`,
`forge-executor`.

Using real crate names means `forge history --component forge-store` and the
Phase 6 world model's component facts line up with the corpus instead of
running in parallel to it.

## `tags`

| Tag | Meaning |
|---|---|
| `validation-campaign` | On every corpus task. The single reliable selector for campaign work. |
| `campaign-v1` | Campaign identity, so a second campaign is separable. |
| `tier2-repetition` | Designated for the optional 3-runs-per-agent variance tier. |
| `context-experiment` | Part of the Phase 6 context A/B subset. |
| `team-candidate` | Decomposable enough for a `forge team` comparison. |
| `health-relevant` | Plausibly moves a Phase 7 health dimension. |
| `policy-evidence` | Expected to produce evidence the Phase 8 optimizer can use. |

Tags are additive selectors, not a hierarchy. A task may carry several.

---

## Category → default evaluator expectations

Not a rule the code enforces; the standard each task in the corpus is held to.

| Category | Required | Advisory / metric-bearing |
|---|---|---|
| `debugging` | `tests`, `lint` | — |
| `feature` | `tests`, `lint` | `custom` contract check |
| `refactor` | `tests`, `lint` | `complexity` |
| `testing` | `tests`, `lint` | `custom` coverage-delta metric |
| `performance` | `tests`, `lint` | `benchmark` (typed metrics, required) |
| `persistence` | `tests`, `lint` | `custom` migration-compatibility check |

`tests` and `lint` are required everywhere because Forge's own release gate is
`cargo test --workspace` and `cargo clippy --workspace --all-targets -D warnings`.
A change that breaks either is not a candidate regardless of what else improved.
