# Repository World Model

Forge’s Repository World Model is a persistent, provider-neutral record of
repository evidence. It complements the immutable task and run ledger: tasks
say what work meant, runs say what happened, and world snapshots say what Forge
could establish about one exact repository commit.

It is not a graph database, compiler frontend, embedding index, repository
health score, or semantic conflict detector. SQLite remains the store and the
entire default build works without a live model or network access.

## Snapshot and commit semantics

`WorldModelSnapshot` contains:

- a monotonic `WM-*` snapshot ID;
- the logical repository name and a full 40-character Git commit;
- creation time, schema version, source, and `Complete`, `Partial`, or `Failed`
  status;
- versioned extractor results; and
- typed facts with stable IDs, evidence confidence, and mandatory provenance.

Snapshots are immutable. `world_model_current` is only a mutable pointer to the
latest accepted complete or partial snapshot for a repository. Rebuilding at a
new commit creates another snapshot; it never edits the old one. Failed
snapshots remain inspectable but do not become current.

Extraction reads the checked-out repository. A build therefore requires both:

1. the requested commit equals the checkout’s resolved `HEAD`; and
2. the checkout is clean.

This prevents uncommitted files, or files from commit B, from being persisted
as exact evidence about commit A. Consumers that compare a snapshot with
another commit receive an explicit relation:

| Relation | Meaning |
|---|---|
| `Exact` | snapshot commit equals the target commit |
| `Ancestor` | the snapshot commit is an ancestor of the target |
| `Stale` | both commits exist, but the snapshot is not an ancestor of the target |
| `UnknownRelation` | Git cannot establish the relation |

Agent, routing, and team integration automatically uses only an exact snapshot.
The CLI may show an older snapshot and reports its relation rather than
presenting it as current truth.

## Typed facts

The provider-neutral core has ten deliberately small fact types:

| Fact | Purpose |
|---|---|
| `Component` | semantic repository area, optional paths/parent/tags/task links |
| `Module` | concrete language module/package/crate and component membership |
| `Interface` | meaningful boundary, visibility, signature/reference, source location |
| `Contract` | explicit, inferred, or historical behavioral expectation |
| `Invariant` | condition expected to remain true and optional evaluator enforcement |
| `Dependency` | typed source→target relationship such as imports, calls, or depends-on |
| `OwnershipRecord` | declared or otherwise explicitly sourced stewardship |
| `PerformanceConstraint` | typed metric, comparison, threshold, and unit |
| `HistoricalDecision` | decision, rationale when known, affected facts, commit, and status |
| `KnownFailureMode` | compact failure description linked to runs/evaluators/commits |

Entity references are validated within each snapshot. A dependency cannot
refer to a missing fact or claim that a module ID is a component ID. Source
paths use `RepositoryPath`, reject absolute/traversal forms, and are stored
relative to the repository. `SourceLocation` binds path, optional symbol/line
range, and exact commit.

Performance constraints reuse Forge’s typed metric name/value and direction
model. Extraction does not invent thresholds. Ownership extraction does not
infer a human owner from Git authorship.

## Stable identity, provenance, and confidence

Facts use `WF-*` IDs derived from the fact kind plus an extractor-defined
logical key—not the display name alone. The Rust extractor keys components by
package identity and modules by repository path. These IDs correlate the same
logical fact across normal metadata changes. Perfect identity across arbitrary
renames and refactors is intentionally unsolved.

Every fact must have at least one typed provenance record identifying the
extractor name/version and evidence source. Available source kinds are:

```text
SourceCode          RepositoryDocument    Configuration
Test                Evaluator             HistoricalRun
CommitHistory       UserDeclared          Imported
AgentInferred
```

Repository locations in provenance must name the snapshot commit. Historical
failure modes refer to existing run IDs rather than copying logs. Component
facts may refer to existing task IDs. Normalized SQLite link tables make those
relationships queryable while the run and task ledgers remain canonical.

Confidence is semantic rather than a fake probability:

- `Declared`: explicitly stated by task/configuration/user evidence;
- `Observed`: deterministically observed in code, manifests, or run history;
- `Inferred`: an advisory derivation;
- `Unknown`: retained evidence whose certainty is not established.

Facts can carry explicit `contradicts` links. Validation preserves both sides
and requires linked facts to exist; Forge does not silently choose a winner.
Phase 6 does not discover semantic contradictions automatically.

## Extractor architecture

`WorldModelExtractor` is an async provider-neutral interface. Extractors
receive a read-only extraction context and return typed `WorldModelFacts`; they
cannot write SQLite. `WorldModelBuilder` runs configured extractors, merges and
canonicalizes facts, validates the candidate, and returns a snapshot plus typed
lifecycle events.

The implemented deterministic extractors are:

### Rust workspace structure

`rust-workspace-structure` statically reads `Cargo.toml` files. It recognizes a
root package, literal workspace members, and trailing `/*` member patterns. It
produces:

- one semantic component and module for each Rust package;
- one public library interface when `src/lib.rs` exists; and
- typed `DependsOn` edges for dependencies between workspace packages.

It does not execute Cargo, build scripts, repository commands, or arbitrary
manifest paths. The language-specific behavior is isolated in `forge-world`;
the core schema remains language-neutral.

### Forge task and history evidence

`forge-task-history` statically reads YAML/JSON under `.forge/tasks`, including
the shared task portion of Phase 5 team files. It produces declared component
links and task constraints as invariants. It also queries the immutable
experience ledger for non-passing runs and creates compact observed failure
modes linked to their run, evaluator, and commit IDs. Large logs remain in the
ledger/artifact layer.

Agent-assisted extraction is not implemented. The schema can represent
`AgentInferred` evidence, but the default build does not spend tokens or make a
model/network call.

## Partial extraction

Each extractor is configured as required or optional:

- all extractors complete → `Complete`;
- an optional extractor fails → valid facts are retained as `Partial`;
- a required extractor fails → `Failed`, retained for diagnosis and excluded
  from the current pointer and exact consumer lookup.

Extractor records include name, version, required flag, status, fact count,
configuration fingerprint, and error when present. The builder emits typed
events with `WorldModelSnapshotId` as their subject: build start, extractor
start/completion/failure, validation, snapshot creation, and build failure.
They are never shoehorned into a run ID.

## CLI

Build at the clean current `HEAD`:

```bash
forge world build
```

Show the current pointer or a historical snapshot:

```bash
forge world show
forge world show WM-0004
```

The report includes commit, time, schema, status, current pointer state,
relation to `HEAD`, extractor results, per-type counts, and provenance counts.

Query one typed fact cohort with an optional case-insensitive term:

```bash
forge world query component storage
forge world query interfaces checkpoint
forge world query dependencies storage
forge world query invariants checkpoint
forge world query failures scheduler
forge world query all --snapshot WM-0004 --limit 100
```

The query is deterministic SQLite filtering over typed records. It does not use
free-form retrieval, embeddings, or an LLM.

## Example

A two-crate repository might create:

```text
Forge world model WM-0001

  Repository        queue-service
  Commit            8a4c… (full hash stored)
  Status            complete
  Relation to HEAD  exact

  Components         2
  Modules            2
  Interfaces         2
  Dependencies       1
  Invariants         1
```

Conceptually, the facts are:

```text
component queue-core       SourceCode/Observed   crates/queue-core/Cargo.toml
component queue-api        SourceCode/Observed   crates/queue-api/Cargo.toml
module queue-core          SourceCode/Observed   crates/queue-core
module queue-api           SourceCode/Observed   crates/queue-api
dependency api → core      SourceCode/Observed   DependsOn
invariant atomic enqueue   UserDeclared/Declared .forge/tasks/T-1042.yaml
```

After adding `queue-worker` and committing, a rebuild creates `WM-0002` bound
to the new commit. `WM-0001` remains byte-for-byte unchanged; the internal diff
API reports added/removed/changed facts and leaves ambiguous identity changes
in `unresolved_identity_changes`. No longitudinal score is computed.

## Existing-system integration

- Ordinary and team-node runs look up an exact snapshot for their base/input
  commit, select at most 12 relevant facts deterministically from task
  components, related task IDs, and objective terms, and append a compact
  provider-neutral prompt section. `AgentRun` records the snapshot and exact
  fact IDs supplied. No snapshot is a normal fallback.
- Routing decisions may record `world_model_snapshot_id`. The field is not in
  the `historical-baseline-v1` request fingerprint and does not alter evidence,
  scoring, readiness, margin, or selected agent.
- Team executions record compact exact context for the root base commit.
  Explicit plans remain valid without it. Every agent-backed node is an
  ordinary run and independently resolves exact context for its input commit.
- World facts link to task and run IDs without duplicating immutable task
  revisions or run histories.

## Persistence and refresh

Migration `0010_world_model.sql` adds snapshot/current, extractor, fact,
fact→run, fact→task, and lifecycle-event tables. Complete typed JSON remains
canonical; normalized columns support commit/current/kind/term/reference
queries. Runs, routing decisions, and teams have nullable snapshot references.
Existing Phase 0–5 rows migrate with `NULL` references and retain their JSON
records and historical meaning.

Refresh is currently a deterministic full rebuild. This favors correct commit
binding over a complex incremental compiler. `WorldModelSnapshot::diff`
compares stable IDs and semantic fact content while ignoring only the snapshot
ID and source-location snapshot commit.

## Configuration and limitations

Only implemented controls are exposed:

```toml
[world_model]
enabled = true
structure = true
task_metadata = true
history = true
```

Important limitations:

- only Rust Cargo workspace structure has a language-specific extractor;
- public Rust interfaces are currently crate-library boundaries, not every
  trait/function/module;
- document/ADR, CODEOWNERS, test-source, and configuration-semantic extractors
  are not implemented yet, though their fact/provenance types exist;
- there is no agent inference, natural-language retrieval, graph database,
  perfect refactor identity, full semantic conflict detection, or automatic
  context optimization;
- partial snapshots are visible and usable only when their exact commit is
  requested; consumers can inspect status before deciding policy;
- Phase 6 computes no health score, trend, intervention, or longitudinal
  optimization. Those belong to Phase 7.
