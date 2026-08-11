# Forge

**A longitudinal control plane for autonomous software engineering.**

Forge is not a coding agent. It sits *above* coding agents — Claude Code, Codex,
Pi, whatever comes next — and treats them as interchangeable engineering
workers. Its job is to assign work, isolate it, measure the result
independently, and remember what happened, so that engineering decisions can
eventually be made from evidence instead of vibes.

The question most systems answer is *did the task pass?* The question Forge is
built to answer is:

> Did this change actually make the software better — and which agent,
> configuration, and context is most likely to make it better next time?

See [`forge_project_plan.md`](forge_project_plan.md) for the full design.

---

## Status

Forge can run a real engineering task through Claude Code or Codex in isolated
worktrees, evaluate each result itself, and group independent attempts from one
base commit into a comparative experiment.

| Capability | State |
|---|---|
| Core domain model (task, run, event, workspace, evaluation, metric) | ✅ |
| Git worktree isolation with safety invariants | ✅ |
| Subprocess execution: timeouts, output caps, process-group cleanup | ✅ |
| Environment policy and secret redaction | ✅ |
| Typed test, benchmark, lint, security, complexity, and custom evaluation | ✅ |
| SQLite experience ledger (runs, trajectories, evaluator results, metrics) | ✅ |
| `forge init`, `forge agent list`, `forge task validate` | ✅ |
| **Claude Code adapter and `forge run`** | ✅ |
| Protected evaluation inputs, candidate patch policy, security posture | ✅ |
| Structured benchmark metric contract | ✅ |
| **Codex adapter and `forge run --agent codex`** | ✅ |
| **`forge compete task.yaml --agents claude,codex`** | ✅ |
| **History, agent statistics, failures, similarity, experiments, JSONL export** | ✅ |
| **Provider-agnostic historical routing and trusted evidence policy** | ✅ Phase 4B review candidate |
| Multi-agent teamwork and later optimization | ⬜ later |

---

## Quickstart

### Prerequisites

- **Rust** 1.93 or newer (`cargo build --release`)
- **Git** 2.5 or newer (worktree support)
- **Claude Code or Codex CLI** on your `PATH`, already authenticated. Check
  exact availability with `forge agent list`.
- A **Git repository with at least one commit**. Agents work from a commit, so
  uncommitted changes are invisible to them.

### Run a task

```bash
forge init
```

```bash
forge run .forge/tasks/my-task.yaml --agent claude
```

or, through the identical Forge pipeline and prompt contract:

```bash
forge run .forge/tasks/my-task.yaml --agent codex
```

`forge agent list` shows which agents Forge can actually run, and
`forge task validate <file>` checks a task before you spend a run on it.

The retained evidence is queryable without writing SQL:

```bash
forge history --agent codex --outcome pass --limit 20
forge agent stats codex
forge failures --component storage --agent codex
forge task similar T-1042
forge experiments list
forge export --format jsonl > forge-runs.jsonl
```

These commands describe recorded evidence. Forge can also apply the
deterministic `historical-baseline-v1` policy to its trusted snapshot with
`forge run task.yaml --agent auto`. It selects only when readiness and the
score margin permit; otherwise it stops explicitly without running an agent.

To run independent attempts from one resolved base and compare their evidence
without choosing an overall winner:

```bash
forge compete .forge/tasks/my-task.yaml --agents claude,codex
```

Competition is sequential in Phase 1B. Each participant is an ordinary Forge
run with its own worktree, patch, evaluation, and trajectory. The experiment
stores their shared base and run links, then reports pairwise dimensional
relationships. One failed or timed-out participant does not erase the others.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | A change was produced and every check passed |
| `1` | Forge could not run the task (bad task, missing CLI, not a repo) |
| `2` | The run completed but the outcome was not a pass |

`1` and `2` are deliberately different: "the tool broke" and "the change didn't
work" call for different responses in a script.

---

## A task

A task separates what the agent is told from what Forge measures. The agent
never gets a say in the second part.

```yaml
task_id: T-1042
repository: distributed-runtime
objective: >-
  Improve checkpoint write throughput without weakening recovery guarantees.

constraints:
  - All existing tests must pass
  - Recovery semantics cannot change
  - Memory increase must remain below 10%

evaluation:
  tests:
    command: cargo test --workspace
    timeout_secs: 900
  lint:
    command: cargo clippy --workspace -- -D warnings
  benchmark:
    command: ./bench/checkpoint.sh
    metrics_file: .forge-metrics.json

protected_paths:
  - tests/**
  - benches/**
# A task that legitimately updates a particular test may grant a narrow exception:
allowed_protected_paths:
  - tests/checkpoint_format.rs

classification:
  category: performance
  language: rust
  domain: storage
  difficulty: hard
components:
  - checkpointing
  - storage
tags:
  - throughput
```

`repository` must match the `name` in `.forge/config.toml`, so a task cannot be
run against the wrong repository by accident.

Classification, components, and tags are optional repository-defined strings
used for cohorts and deterministic similarity. Forge never asks an LLM to infer
them. Existing `metadata.task_type`, `metadata.language`, and
`metadata.subsystem` fields remain compatible as category, language, and domain
fallbacks.

Each run is bound to an immutable content-addressed snapshot of the complete
task definition. Reusing a task ID with edited instructions or classification
affects future runs only; historical queries and exports retain the revision
that each older run actually executed.

See [`docs/evaluation.md`](docs/evaluation.md) for every evaluator category,
required versus optional policy, structured metric output, and trust rules.
See [`docs/experience-ledger.md`](docs/experience-ledger.md) for query semantics,
similarity weights, missing-data rules, and the JSONL schema.
See [`docs/routing.md`](docs/routing.md) for pre-run features, provenance,
evidence eligibility, readiness, decision contracts, and reproducibility.

---

## What a run does

```text
validate task → resolve base commit → create run record
      ↓
create isolated Git worktree from that commit
      ↓
invoke the selected coding agent with the task contract    ← untrusted
      ↓
──────────────────── TRUST BOUNDARY ────────────────────
      ↓
read the full workspace delta relative to the base commit
      ↓
apply candidate patch policy and check protected evaluation inputs
      ↓
run the task's own checks against the workspace
      ↓
derive the outcome → persist run, events, patch, metrics
```

The report keeps the two judgments apart, because they are different claims
from different sources:

```text
Forge run R-0001

  Task         T-0001  Implement the `median` function in src/lib.rs …
  Agent        claude (claude-code)
  Base commit  83469ef
  Branch       forge/R-0001
  Workspace    .forge/worktrees/R-0001 (removed)

Agent execution
  Status     completed
  Duration   18s
  Exit code  0
  Tokens     164,094 (163,034 in / 1,060 out)
  Cost       $0.1367

Patch
  2 files changed, 20 lines (+19 / -1)
  .forge/runs/R-0001/patch.diff
  committed as 37f89c4

Evaluation integrity
  clean

Evaluation (run by Forge, not by the agent)
  EVALUATOR  CATEGORY  POLICY    RESULT  EXECUTION
  tests      test      required  PASS    completed  exit 0, 97ms
  lint       lint      required  PASS    completed  exit 0, 125ms

  Required          2
  Optional          0
  Metrics           2
  Execution errors  0

Overall
  PASS
```

### Three statuses, never one

A run records what the pipeline did, what the *process* did, and what Forge
concluded — separately. They diverge in ways that matter:

| Agent exited | Candidate patch | Checks | Integrity | Outcome |
|---|---|---|---|---|
| non-zero | present | pass | clean | `PASS` |
| zero | present | fail | clean | `FAIL` |
| timed out | present | pass | clean | `PASS` |
| zero | empty | pass | clean | `NO CHANGE` |
| zero | present | pass | protected test deleted | `INCONCLUSIVE` |
| zero | present | none configured | clean | `INCONCLUSIVE` |
| could not start | — | — | — | `ERROR` |

An unchanged repository passes its own tests trivially, so **producing no
change is never a pass**. Equally, an agent that crashed after writing a
correct patch is not penalized for crashing — Forge judges the artifact, not
the process.

---

## Where run data lives

Everything is under `.forge/` in the repository:

```text
.forge/
├── config.toml          # configuration          (commit this)
├── tasks/               # task definitions       (commit these)
├── forge.db             # the experience ledger  (ignored)
├── worktrees/<run-id>/  # agent workspaces       (ignored)
└── runs/<run-id>/       # per-run artifacts      (ignored)
    ├── prompt.txt           # exactly what the agent was asked
    ├── agent.stdout.log     # captured agent output
    ├── agent.stderr.log
    ├── patch.diff           # the change, read out of Git
    └── checks/<name>.log    # full output of each check
```

`forge init` writes `.forge/.gitignore` so run output never enters your history
while configuration and tasks do.

Each run also leaves a branch, `forge/<run-id>`, holding the agent's work as a
commit. The workspace directory is removed after a clean run; the branch is
not, so the change is always recoverable:

```bash
git diff main..forge/R-0001
```

The ledger is plain SQLite. Prefer the typed CLI query surface for stable
automation:

```bash
forge history --repository distributed-runtime
forge export --format jsonl
```

Direct inspection remains possible:

```bash
sqlite3 .forge/forge.db "SELECT run_id, status, agent_status, outcome, cost_usd FROM runs"
```

Evaluator results are normalized too:

```bash
sqlite3 .forge/forge.db \
  "SELECT run_id, evaluator_id, kind, required, verdict, execution_status FROM evaluator_results"
```

### Configuration

Historical baseline routing is configured conservatively:

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

```toml
[agents.claude]
executable = "claude"          # for a non-standard install
model = "opus"
timeout_secs = 1800
permission_mode = "acceptEdits"
```

```toml
[agents.codex]
executable = "codex"
model = "gpt-5-codex"
timeout_secs = 1800
sandbox_mode = "workspace-write"
approval_policy = "never"
extra_args = ["--ephemeral"]
```

The inspected Codex command, JSONL metadata contract, and security mapping are
documented in [`docs/codex-cli.md`](docs/codex-cli.md).

`executable`, `model`, `timeout_secs`, `extra_args`, and
`execution_provenance` are typed Forge settings. Other keys under
`[agents.<id>]` are passed to that adapter unchanged.

Normal CLI executions are explicitly recorded as `live`. Deterministic local
stub configurations must declare `execution_provenance = "synthetic"`; older
database rows migrate to `unknown`, never guessed `live`.

---

## Security limitations

Read this before pointing Forge at anything you care about.

**Candidate changes are isolated in a Git worktree; Forge does not independently
contain agent processes.** Each run starts in a disposable worktree, so ordinary
relative edits produce a separate candidate and do not alter the primary checkout. A
Git worktree is not a sandbox: the agent can use absolute or parent paths and
write anywhere your user account can. Container isolation is the fix, and it
is not built yet. Every run report states this posture explicitly.

**Claude Code runs with `bypassPermissions` by default.** An unattended agent
cannot answer a permission prompt, and anything stricter leaves it unable to run
the build and test commands its instructions ask for. Set
`permission_mode = "acceptEdits"` under `[agents.claude]` to tighten it, at the
cost of the agent being unable to run commands.

**Codex runs with its `workspace-write` sandbox and `never` approval policy by
default.** This is intentionally not reported as Forge host containment. Codex
constrains model-generated commands to its workspace boundary, while the CLI
process itself still runs as the invoking user and Forge has not placed it in a
container. `danger-full-access` is configurable but is reported as unrestricted
and triggers Forge's unconfined-run warning.

**Consequently: run Forge only on repositories and tasks you would be willing
to run by hand, on a machine where that is acceptable.**

What Forge *does* guarantee, with tests:

- It only ever creates or destroys directories inside its configured worktree
  root, and rejects any run or check name that could escape it.
- Credentials are filtered out of the environment agents and checks inherit;
  only the specific variables each selected harness needs are allowed back in,
  and secret-looking values are redacted from captured output before it is stored.
- Evaluation commands run with a conservative environment — they execute code
  an agent just wrote, and have no business seeing credentials.
- Protected evaluation inputs are compared with the recorded base commit;
  additions, modifications, deletions, and task-scoped exceptions are persisted.
- Ignored build output, Forge runtime files, Git internals, and oversized files
  are not candidate patch content. Binary additions remain visible but carry a
  structured warning.
- Anything written outside the workspace is not captured in the patch and never
  credits the run.

---

## Architecture

```text
forge-cli ── forge-runner (execution pipeline)
             ├── forge-agent      provider adapters
             ├── forge-eval       evaluation trust boundary
             ├── forge-executor   process/workspace isolation
             ├── forge-store      experience ledger
             └── forge-git        repository mechanics

forge-router (historical-baseline-v1 selection and explanation)
└── forge-store          trusted routing evidence query

All provider-agnostic domain contracts converge on forge-core.
```

| Crate | Responsibility |
|---|---|
| `forge-core` | The vocabulary. No agent, no database, no execution. |
| `forge-git` | Repositories, worktrees, diffs. |
| `forge-executor` | Process execution, environment policy, workspace provisioning. |
| `forge-agent` | The `AgentAdapter` interface, shared prompt contract, and provider adapters. |
| `forge-eval` | Independent evaluation — the trust boundary. |
| `forge-store` | The SQLite experience ledger. |
| `forge-router` | Candidate resolution, trusted evidence, and the versioned historical baseline. |
| `forge-runner` | The run pipeline. The engine a CLI, API, or scheduler each drives. |
| `forge-cli` | The `forge` binary. |

Everything provider-specific lives in its adapter file:
[`claude.rs`](crates/forge-agent/src/claude.rs) and
[`codex.rs`](crates/forge-agent/src/codex.rs). The prompt is built by
[one shared function](crates/forge-agent/src/prompt.rs) with no agent parameter
— two agents given different instructions could not be meaningfully compared.

### Invariants worth knowing

These are enforced in code and covered by tests, not just documented.

**Forge never trusts an agent's account of its own work.** The patch is read
from Git; the verdict comes from commands the repository declared in advance.
What the agent claimed is recorded as trajectory data and consulted by nothing —
including Claude's own `is_error` flag, which is stored as metadata and never
becomes a status.

**Green checks require intact evaluation inputs.** The trusted task definition
is loaded before agent execution. Protected paths are compared against the base
commit, and a green check cannot produce `PASS` after an unapproved protected
file addition, modification, or deletion.

**A workspace delta is not automatically a candidate patch.** Forge respects
Git ignore rules, excludes Forge-owned and oversized artifacts with recorded
reasons, flags binary additions, and commits only the policy-approved candidate
to the durable run branch.

**Missing evidence is not a pass.** An evaluator that could not execute is
recorded as an execution error with an `Inconclusive` result, distinct from a
tool that executed and returned `Fail`. Required evaluators determine the
overall verdict; optional evaluator results remain visible but do not block a
pass. Existing task files default every evaluator to required.

**Raw measurements are never discarded.** Evaluations store raw metrics in
their original units alongside normalized dimensions, and there is deliberately
no single overall score: weightings will change as evidence accumulates, and
they should be recomputable from history rather than requiring re-runs.

**Trajectories, not outcomes.** Runs are recorded as ordered event streams,
because that is the raw dataset a routing model will eventually learn from.
Forge records only events it can actually observe — it does not fabricate
fine-grained `FileRead`/`FileModified` events that the agent interface does not
expose.

---

## Development

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

The test suite never invokes a real model or touches the network. Pipeline
tests drive a fake `AgentAdapter`; CLI tests drive the real binary and real
adapters against local stub executables.

To smoke-test against the real Claude Code:

```bash
./fixtures/new-fixture-repo.sh median /tmp/median && cd /tmp/median && forge init
```

```bash
forge run task.yaml --agent claude
```

The same controlled fixture can be run with Codex:

```bash
forge run task.yaml --agent codex
```

Or as one controlled infrastructure experiment from an identical base:

```bash
forge compete task.yaml --agents claude,codex
```

The fixture ships four failing tests and an unimplemented function, so a
`PASS` requires the agent to have done real work. Its task declares test, lint,
security, complexity, benchmark, and custom evaluators. The automated pipeline
suite runs the same six-dimensional shape through a deterministic adapter, then
changes only security to verify that just that evaluator flips to `FAIL`.
