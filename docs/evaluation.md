# Evaluation framework

Forge resolves an immutable evaluation plan from the trusted task file before
the coding agent starts. After the candidate patch is captured, one evaluation
engine runs every declared evaluator in order and preserves partial evidence if
another evaluator fails. The agent's self-assessment is never an evaluation
input.

## Evaluator categories

All categories are repository-configured commands; Forge does not guess an
ecosystem or hard-code a particular scanner.

| Task key | Category | Result source |
|---|---|---|
| `tests` | Test | command exit status |
| `benchmark` | Benchmark | exit status and optional structured metrics |
| `lint` | Lint | command exit status |
| `security` | Security | command exit status; the configured tool owns finding/severity policy |
| `complexity` | Complexity | command exit status and optional structured metrics |
| `custom` | Custom | command exit status and optional structured metrics |

The existing `build` key remains supported as a compatibility category.

```yaml
evaluation:
  tests:
    command: cargo test --workspace
    timeout_secs: 900
  lint: cargo clippy --workspace --all-targets -- -D warnings
  security:
    command: ./scripts/security-check.sh
    required: false
  complexity:
    command: ./scripts/complexity.sh
    metrics_file: .forge-complexity.json
  benchmark:
    command: ./bench/run.sh
    metrics_file: .forge-benchmark.json
    timeout_secs: 1800
  custom:
    - id: api_contract
      command: ./scripts/api-contract.sh
    - id: source_stats
      command: ./scripts/source-stats.sh
      metrics_file: .forge-source-stats.json
      required: false
```

Every command may set a repository-relative `working_dir`, an evaluator-specific
`timeout_secs`, and `required`. The default is `required: true`, preserving the
behavior of existing task files. Any failed required evaluator fails the
evaluation; any inconclusive required evaluator makes it inconclusive. Optional
results and metrics are always stored but do not change the overall verdict.
Forge does not apply weights or compute a single quality score.

Custom `id` values must be unique, safe identifiers and cannot shadow built-in
evaluator IDs. The earlier `name` spelling remains accepted for compatibility.

## Structured metrics

Benchmark, complexity, and custom evaluators can declare `metrics_file`. The
command must write JSON with this exact shape:

```json
{
  "metrics": {
    "throughput": {
      "value": 4720.3,
      "unit": "MB/s",
      "direction": "maximize"
    },
    "branch_points": {
      "value": 12,
      "unit": "points",
      "direction": "minimize"
    }
  }
}
```

`direction` is `maximize`, `minimize`, or `neutral`. Values must be finite;
metric names must be printable and at most 128 characters; units, when present,
cannot be empty. Forge keeps values in their original units and never performs
implicit conversion. Competition reports incompatible units or directions as
not comparable.

The metrics file must remain inside the repository and may not traverse a
symlinked parent. Forge deletes stale candidate-written output immediately
before the evaluator command, rejects a symlinked result, and treats missing,
empty, malformed, or invalid structured output as inconclusive. Configured
metrics files are excluded from candidate patches.

## Result and execution status

Each result records evaluator ID and category, requiredness, `PASS`/`FAIL`/
`INCONCLUSIVE`, evaluator execution status, duration, command, exit code,
captured-output artifact, metrics, warnings, and any execution error.

A tool that runs and exits nonzero is a valid `FAIL` measurement. Failure to
start the evaluator is an execution error and yields `INCONCLUSIVE`; it does not
become evidence that the candidate is wrong. A timed-out command is a completed
negative measurement because it exceeded the task's declared budget. Later
evaluators still run in every case.

## Trust and security

The task configuration is loaded and validated outside the candidate worktree,
and the plan is resolved before agent execution. Evaluators receive trusted
task, repository, base-commit, workspace, patch, configuration, artifact,
process-runner, and timeout context. They never reread a task definition written
by the candidate.

Evaluation commands run candidate code with Forge's conservative environment
and secret filtering, but Forge does not provide host containment. A Git
worktree isolates repository state; it is not a process sandbox. Configure only
commands you are willing to run as the invoking user.

The SQLite ledger stores complete typed results plus normalized rows in
`evaluator_results` and raw measurements in `metrics`. Lifecycle events include
`EvaluationStarted`, `EvaluatorStarted`, `EvaluatorCompleted` or
`EvaluatorFailed`, and `EvaluationCompleted`. Every lifecycle event carries a
typed `EvaluationSubject`: `Run(RunId)` for an ordinary run or
`TeamExecution(TeamExecutionId)` for the independent evaluation of an
integrated team candidate. Legacy run events without the field are read as a
`Run` subject using their existing run envelope. Run lifecycle payloads remain
in the existing `events` table; team-final lifecycle payloads use the existing
`team_events` table, so this abstraction requires no schema migration.
