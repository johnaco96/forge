# Team execution

Phase 5 adds task-driven multi-agent coordination without introducing fixed
personas. A team is a validated DAG of engineering subtasks, dependencies,
capability requirements, agent assignments, immutable artifacts, candidate
commits, and reviews.

```bash
forge team .forge/tasks/checkpoint-team.yaml
```

`forge team` does not mean “run several agents and trust consensus.” Each
agent-backed node is an ordinary Forge run. Forge independently evaluates the
one final candidate after scheduling and integration.

## Complete plan example

The `team` key is transport for an explicit `TeamPlan`. Forge removes it before
creating the immutable root `TaskRevision`, so an ordinary task file and a team
task file with identical task fields describe the same root revision.

```yaml
task_id: T-2042
repository: distributed-runtime
objective: >-
  Remove checkpoint contention without weakening recovery semantics.

constraints:
  - Recovery semantics must remain unchanged

classification:
  category: debugging
  language: rust
  domain: concurrency
  difficulty: medium
components:
  - checkpointing

evaluation:
  tests:
    command: cargo test --workspace
  lint:
    command: cargo clippy --workspace --all-targets -- -D warnings

protected_paths:
  - tests/**

team:
  plan_version: team-plan-v1
  nodes:
    - id: inspect
      objective: Identify where checkpoint locks are held across I/O
      execution: analysis
      outputs:
        - structured_findings
      assignment:
        strategy: explicit
        agent: claude

    - id: implement
      objective: Implement the smallest correction supported by the findings
      execution: implementation
      depends_on:
        - inspect
      inputs:
        - structured_findings
      outputs:
        - candidate_patch
        - candidate_commit
        - evaluation
      capabilities:
        - edit_files
      assignment:
        strategy: auto

    - id: review
      objective: Review recovery semantics and the declared constraints
      execution: review
      depends_on:
        - implement
      inputs:
        - candidate_patch
        - candidate_commit
        - evaluation
      outputs:
        - review
      assignment:
        strategy: explicit
        agent: codex

    - id: final
      objective: Select the reviewed linear candidate for final evaluation
      execution: integration
      depends_on:
        - review
```

`node_id` is the canonical field name; `id` is accepted as a concise YAML
alias. Unknown node fields are rejected. Agent-backed nodes require an
assignment. Deterministic `integration` and `verification` nodes must not have
one.

## Plan and DAG contract

`TeamPlan` contains the plan version, exact root objective, and typed nodes.
Dependencies are declared with `depends_on`; validated plans also retain the
normalized edge list and deterministic topological order.

Validation occurs before a team ID, workspace, or agent process is created. It
rejects:

- unsupported versions, empty plans, or empty objectives;
- duplicate node IDs and duplicate dependency edges;
- missing dependencies, self-dependencies, and cycles;
- missing or unexpected assignment strategies;
- declared inputs with no dependency; and
- input artifact kinds not promised by a direct dependency.

The stable SHA-256 plan fingerprint covers normalized nodes, objectives,
dependencies, execution types, constraints, capabilities, assignments, inputs,
outputs, and required-node policy. Reordering nodes or dependency lists does
not change it; changing plan semantics does.

`TeamPlanner` is provider-agnostic. `StaticTeamPlanner` is the Phase 5 path used
by the CLI and tests. The persisted `PlanProvenance` distinguishes `explicit`,
`generated`, and `imported` plans and can retain planner configuration and a
planner-run reference. Phase 5 does not require a model to plan and does not
expose an agent-backed planner in the CLI.

## Node semantics and lineage

Execution types describe work, not personas:

| Type | Phase 5 behavior |
|---|---|
| `analysis` | Ordinary agent run with no patch requirement or node evaluator; must return structured JSON or retained prose. `NoChange` is valid. |
| `implementation` | Ordinary Forge run with the inherited commit, shared prompt machinery, patch policy, integrity checks, and root evaluation. Must produce a passing committed candidate. |
| `review` | Ordinary no-edit agent run against the inherited candidate. Produces `Approve`, `RequestChanges`, or `Inconclusive` evidence. |
| `integration` | Deterministic pass-through for one unambiguous input commit. |
| `verification` | Deterministic pass-through; the authoritative verification remains the mandatory final evaluation. |

Every agent node receives a derived immutable task with a unique node task ID.
Its persisted `NodeTaskLineage` records the root task and revision, team and
node IDs, node objective, input commit, supplied artifact IDs, inherited root
constraints, and node-specific constraints. The root revision is never
mutated.

Node prompts use the existing shared deterministic coding-agent prompt. Claude
and Codex receive the same node semantics. Only artifact kinds explicitly
listed in `inputs` and produced by direct dependencies are included; Forge does
not paste all prior output into every prompt.

## Assignment and routing

An explicit assignment resolves the normal registered agent configuration and
checks executable availability and required capabilities before execution.

An `auto` assignment calls the Phase 4 `RoutingContract` with the node task,
candidate configurations, capability requirements, evidence policy, historical
cutoff, and configured readiness thresholds. The selected configuration,
fingerprint, `SelectionSource`, and routing-decision ID are persisted. If the
router returns `InsufficientEvidence` or `CompeteRecommended`, the node becomes
`assignment_blocked`; Forge does not choose a fallback or silently start a
competition. The stopping routing decision remains linked to the node.

## Isolation and handoffs

Every agent-backed node executes through `Runner::execute`, so it gets an
ordinary run ID, branch, isolated Git worktree, captured stdout/stderr,
trajectory, patch, integrity result, evaluation, usage, and provenance.
Independent nodes never share a mutable working tree.

Git worktrees provide workspace isolation, not host containment. Agent
permission/sandbox reporting and host-security limitations are unchanged from
ordinary runs.

Communication is explicit:

- `TeamArtifact` has a durable ID, producer node, typed kind, content/reference,
  creation time, and immutable content hash;
- downstream prompts receive only declared artifact references;
- code inheritance uses the predecessor candidate commit as the next ordinary
  run’s exact base revision; and
- no hidden agent chat or shared conversational memory exists.

Artifacts may contain structured findings, analysis prose, candidate patch
metadata, candidate commits, Forge evaluation evidence, review results,
metrics, file references, or
integration commits. Published artifact content hashes cannot be rewritten.

## Scheduler and failure behavior

The initial scheduler is sequential and deterministic. It walks the validated
topological order, marks nodes `pending → ready → running`, persists each state,
and then records `succeeded`, `failed`, `blocked`, or `assignment_blocked`.

When a dependency fails, its dependents are blocked. Independent ready siblings
continue unless `team.stop_on_required_node_failure = true`. Partial ordinary
runs, artifacts, and events remain in the ledger. Failures distinguish agent
process, infrastructure, engineering/evaluation, assignment, integration, and
review causes. Phase 5 adds no autonomous retry policy.

## Review, integration, and final truth

Review is a first-class DAG node. Structured review JSON is parsed strictly;
unstructured output is retained as prose with an `Inconclusive` decision.
Review nodes that modify files fail. `Approve` cannot override failing
integrity or evaluator evidence. `RequestChanges` conservatively prevents a
team pass even when machine checks pass.

Phase 5 safely supports linear candidate chains and one unique terminal
candidate lineage. A deterministic integration node can pass through one input
commit. Distinct parallel candidate commits produce an explicit integration
conflict; Forge does not guess a merge or silently resolve semantic conflicts.

The resulting `FinalCandidate` records root base, integrated commit,
contributing nodes/runs, patch summary, and commit lineage. Forge provisions a
fresh final worktree and independently applies the existing `PatchPolicy`,
protected-input integrity checks, `EvaluationPlan`, and `EvaluationEngine`
against the complete root-to-candidate delta. Node success alone can never make
the team pass.

## Persistence, events, and comparison

Migration `0009_team_executions.sql` adds normalized tables for team
executions, nodes, edges, immutable artifacts, ordinary run links, and team
events. Full JSON snapshots preserve the typed plan and evolving result. Store
guards prevent changing a historical team’s root revision, base commit, plan,
plan provenance, or creation time.

The team record also keeps a raw resource summary: agent-run count, failed
attempts, warning count, summed run duration, reported tokens, and known
provider cost. Missing usage or cost remains unavailable rather than becoming
zero.

Orchestration events include plan resolution, node readiness/start/completion,
failure/blocking, artifact publication, handoff, review, integration, final
evaluation, and team completion. The complete final evaluator lifecycle is
stored in this stream with
`EvaluationSubject::TeamExecution(team_execution_id)`. Ordinary node-run
evaluations retain `EvaluationSubject::Run(run_id)` in their run trajectories;
those events are not duplicated.

After node execution, Forge looks for the latest ordinary non-team run with the
same root task revision, base commit, evaluation semantics, and execution
provenance. If present, the report compares correctness, runtime, tokens, known
cost, integrity, patch size, warnings, and like-for-like benchmark metrics.
Otherwise it reports
`single-agent baseline unavailable`. The comparison records dimensions; it
does not declare teams generally superior or add a team preference bonus.

Synthetic node configurations must use the existing
`execution_provenance = "synthetic"` setting. The aggregate team provenance is
synthetic only when its ordinary node runs are synthetic. Synthetic runs remain
ineligible for live Phase 4 routing evidence.

## Configuration and limitations

```toml
[team]
max_parallel_nodes = 1
stop_on_required_node_failure = false
```

Phase 5 limitations are intentional:

- scheduling is sequential, even for independent ready nodes;
- the CLI accepts explicit embedded plans; generated/imported provenance exists
  in the domain model, but no live model planner is exposed;
- parallel candidate branches require an explicit future integration strategy;
- no semantic merge/conflict engine, distributed scheduler, or autonomous
  retry policy is included.

Final team evaluation uses a detached Forge-owned worktree, stores its result
on `TeamExecution`, and never allocates or persists an ordinary run identity.

Repository architecture modeling and semantic conflict detection belong to
Phase 6 and are not part of this implementation.
