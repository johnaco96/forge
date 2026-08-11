# Experience ledger and institutional memory

Forge keeps task definitions, runs, process status, candidate patches,
integrity evidence, evaluations, raw metrics, trajectories, warnings,
experiments, and artifact references in one SQLite ledger. Phase 3 adds indexed
classification fields and a typed query layer over that existing evidence. It
does not create a second history store or duplicate evaluation records.

Logical task IDs are mutable authoring identities, but historical evidence is
not. Every distinct serialized task definition is stored as a content-addressed
immutable task revision. A run is bound to that revision when its initial
ledger record is created, and subsequent lifecycle updates cannot change the
binding. Editing `T-1042` later therefore changes only its current revision and
future runs; history, cohorts, failures, similarity candidates, and exports keep
the classification and semantics that the older run actually used.

Migration 0006 creates one `legacy:<task-id>` revision from each task snapshot
available in an older database and binds its existing runs to that revision.
This preserves every stored field safely. A pre-migration database that had
already overwritten a task row cannot reconstruct earlier values that were
never persisted; immutable revision capture prevents that loss for all runs
created after migration.

Migration 0007 adds explicit execution provenance. Historical rows become
`unknown` because Forge cannot determine whether an old executable was genuine
or a stub without guessing. New normal CLI runs are `live`; deterministic test
stubs declare `synthetic`. Provenance is immutable once a run is inserted.
These values affect routing trust policy only—ordinary Phase 3 queries continue
to include every provenance.

All historical commands are read-only. They describe what Forge recorded; they
do not recommend an agent or influence execution.

## Task classification

Tasks may declare a small repository-owned vocabulary:

```yaml
classification:
  category: bugfix
  language: rust
  domain: authentication
  difficulty: medium
components:
  - api
  - token-parser
tags:
  - regression
  - customer-reported
```

Every value must be nonempty after trimming, contain no control characters, and
be at most 64 characters. Components and tags cannot contain duplicates.
Values are free-form because Forge does not impose a global taxonomy. No model
or agent classifies tasks automatically.

The fields are optional. For Phase 0-2 task files, `metadata.task_type`,
`metadata.language`, and `metadata.subsystem` remain effective fallbacks for
category, language, and domain. Database migration backfills those indexed
fields, so old ledger rows are immediately queryable. Missing fields remain
missing; Forge does not substitute a guessed value.

## Historical commands

### Run history

```bash
forge history \
  --agent codex \
  --outcome fail \
  --task T-1042 \
  --repository distributed-runtime \
  --experiment E-0001 \
  --category bugfix \
  --language rust \
  --domain authentication \
  --difficulty medium \
  --component token-parser \
  --tag regression \
  --from 2026-01-01T00:00:00Z \
  --through 2026-12-31T23:59:59Z \
  --limit 50
```

Filters combine with AND. Outcomes accept `pass`, `fail`, `inconclusive`,
`no-change`, and `error` (the persisted forms are also accepted). Results are
newest first, with run ID as the deterministic tie-breaker. The default limit
is 20; the internal query layer caps requested limits at 10,000.

### Agent statistics

```bash
forge agent stats codex
```

The summary reports outcome counts and `PASS / all recorded runs`. Runs without
a final outcome remain in the denominator and are shown as unresolved. Runtime
is the measured agent-process duration, not queueing or evaluation time.
Provider-reported tokens are stored input plus output tokens. Patch size is
insertions plus deletions. Integrity violations count runs whose integrity is
not clean.

Numeric medians sort the available observations; an even sample count uses the
arithmetic mean of the two middle values. Token, cost, runtime, and patch
summaries state both the sample count and total run count. Missing provider
usage or cost is `unavailable`, never zero. Cost summaries are explicitly the
known total and known median, not estimates for unreported runs. Category and
component cohorts use the same `PASS / total cohort runs` rule.

### Failure investigation

```bash
forge failures --component storage --agent codex --category performance
```

This lists `FAIL`, `INCONCLUSIVE`, `NO CHANGE`, and `ERROR` outcomes. Each entry
includes failed or inconclusive evaluator results, evaluator execution errors,
integrity status and violations, structured warnings, total run duration, base and
candidate commits, and references to retained artifacts. Filters combine with
AND and the default limit is 20.

### Similar tasks

```bash
forge task similar T-1042 --limit 10
```

Similarity is deterministic, fixed-weight, and explainable. There are no
embeddings or LLM calls. A candidate receives:

| Matching evidence | Maximum contribution |
|---|---:|
| repository | 0.20 |
| category | 0.20 |
| language | 0.15 |
| domain | 0.15 |
| difficulty | 0.10 |
| component Jaccard overlap | 0.10 |
| tag Jaccard overlap | 0.05 |
| objective-token Jaccard overlap | 0.05 |

Only present, matching fields contribute. Objective tokens are lowercase
alphanumeric words of at least three characters. Results sort by descending
score, task ID, and revision ID. The target uses the logical task's most recent
run-bound revision, falling back to its current revision only if it has never
run. Thus an unexecuted edit cannot rewrite similarity evidence. Candidates are
immutable revisions that were actually bound to historical runs. Output names
the candidate revision and every matched feature, and may report each agent's
outcome counts for that exact revision. Those counts are facts, not an agent
recommendation.

### Experiments

```bash
forge experiments list --limit 20
```

Experiment history retains the shared task, repository, base commit,
participants, lifecycle status, duration, and links to ordinary Forge runs.
Run evidence remains canonical in the run records and is not copied into the
experiment.

## JSON Lines export

```bash
forge export --format jsonl > forge-runs.jsonl
```

The command emits one chronological normalized record per run. Each line is an
independent JSON object with `schema_version: 1` and includes:

- complete immutable task definition plus indexed identity, objective,
  repository, classification, components, and tags;
- the immutable task revision ID bound to the run;
- exact agent/harness/model/tools/settings configuration;
- explicit live, synthetic, imported, or unknown execution provenance;
- run status, separate agent execution status, Forge outcome, and failure
  reason;
- integrity evidence, evaluation results, evaluator metrics, and warnings;
- agent runtime, provider-reported token fields, known cost, and patch summary;
- run/experiment IDs, base commit, timestamps, and artifact path references.

Unavailable measurements serialize as JSON `null` or an absent optional field
according to the typed record; they are never rewritten as zero. Full prompts,
stdout, stderr, check logs, trajectories, and diffs are not embedded. Their
paths are exported as references, keeping JSONL suitable for offline analysis
without silently producing enormous records.

## Phase boundary

Phase 3 makes historical evidence retrievable and inspectable. Phase 4 adds a
separate conservative routing-evidence policy and deterministic baseline over
that ledger; synthetic and unknown evidence remain queryable here but are
excluded from production routing by default. See [`routing.md`](routing.md).
Forge still does not add performance recommendations, automatic task
classification, multi-agent teamwork, PostgreSQL, a graph database, or a web
dashboard.
