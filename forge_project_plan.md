# Forge
## A Longitudinal Control Plane for Autonomous Software Engineering

**Status:** Historical design plan; Phases 0–8 are implemented
**Primary goal:** Build an AI-native engineering control plane that can run coding agents, independently evaluate their work, retain structured experience over time, and learn which agent/configuration performs best for different engineering tasks.

> Historical context (updated 2026-08-22): this document records the design
> sequence that guided Forge's implementation. It is not the current execution
> plan, and instructions below such as “start with” or “do not begin” describe
> the original roadmap. Phases 0–8 now exist in the product. Current v1.1.0
> release status and remaining gates are maintained in
> `docs/production-readiness.md` and `docs/releases/v1.1.0.md`.
>
> Release closure (2026-08-22): the RC4 product is locally ready for supervised
> production under an explicit human waiver after 7/7 executed pilot tasks
> passed and two frozen tasks were not attempted. The original nine-outcome
> gate remains not fully satisfied. GitHub CI/CD and publication are external,
> and autonomous production is not authorized.

---

# 1. Executive Summary

Forge is **not** intended to be another coding agent, generic agent harness, or multi-agent orchestration framework.

Instead, Forge sits **above** coding agents and agent harnesses such as:

- Claude Code
- OpenAI Codex
- Pi
- future local or specialized agents

Forge treats agents as interchangeable engineering workers.

Its job is to:

1. assign engineering tasks,
2. create isolated workspaces,
3. execute coding agents,
4. capture their complete work trajectories,
5. independently evaluate the resulting code,
6. compare engineering outcomes,
7. persist structured experience,
8. learn which agent/model/harness/tool configuration works best for different task types,
9. eventually coordinate multi-agent teams,
10. eventually measure and optimize repository health over long time horizons.

The central thesis is:

> **The next important infrastructure layer is not another coding agent. It is a system that can measure engineering outcomes across agents and over time, learn from those outcomes, and improve future engineering decisions.**

---

# 2. Core Problem

Modern coding agents are increasingly capable of editing real repositories, running tools, debugging code, and completing software-engineering tasks.

However, most systems still optimize for a short-term question:

> Did the task pass?

That is insufficient.

A change may pass tests today while degrading:

- maintainability,
- memory usage,
- latency,
- architecture,
- security,
- dependency complexity,
- build time,
- future agent performance,
- long-term repository health.

Forge should instead answer:

> **Did this change actually make the software better?**

And eventually:

> **Which agent, model, harness, context strategy, and tool configuration is most likely to produce the best engineering outcome for this kind of task?**

---

# 3. Long-Term Vision

The eventual system looks like this:

```text
                    HUMAN OBJECTIVE
                          │
                          ▼
                ┌──────────────────┐
                │      FORGE       │
                │  CONTROL PLANE   │
                └─────────┬────────┘
                          │
         ┌────────────────┼────────────────┐
         ▼                ▼                ▼
      Codex          Claude Code           Pi
         │                │                │
         ▼                ▼                ▼
      changes          changes          changes
         │                │                │
         └────────────────┼────────────────┘
                          ▼
                  ┌──────────────┐
                  │ Verification │
                  ├──────────────┤
                  │ correctness  │
                  │ performance  │
                  │ architecture │
                  │ security     │
                  │ quality      │
                  │ cost         │
                  └───────┬──────┘
                          ▼
                   EXPERIENCE STORE
                          │
             ┌────────────┴────────────┐
             ▼                         ▼
       What worked?              What failed?
             │                         │
             └────────────┬────────────┘
                          ▼
                  FUTURE DECISIONS
```

Forge should eventually behave less like a task runner and more like a **scheduler for machine engineering capability**.

---

# 4. What Forge Is Not

Forge should **not** begin as:

- another chatbot,
- another RAG framework,
- another MCP wrapper,
- another generic harness,
- another LangGraph clone,
- another CrewAI clone,
- another hard-coded “CEO / architect / developer / tester” multi-agent demo,
- another thin wrapper over one model provider.

The value is in the layer **above the agents**:

- measurement,
- evaluation,
- experience,
- routing,
- longitudinal repository health,
- agent comparison,
- context optimization,
- semantic conflict detection,
- adaptive orchestration.

---

# 5. First Principle: Start With One Agent Run

Do **not** begin with a complicated multi-agent architecture.

The first useful capability is:

> Given a Git repository and an engineering task, Forge can run one real coding agent in an isolated workspace, record exactly what happened, independently evaluate the resulting change, and persist the run.

This gives us a clean foundation.

---

# 6. Forge MVP Definition

The first meaningful Forge MVP is:

> Given the same software-engineering task, Forge can run two coding agents independently from the same repository state, evaluate both results, and determine which result produced the better engineering outcome.

```text
                  Engineering Task
                         │
                         ▼
                 ┌───────────────┐
                 │     FORGE     │
                 └───────┬───────┘
                         │
               isolated workspaces
                 ┌───────┴───────┐
                 ▼               ▼
             Claude Code       Codex
                 │               │
                 ▼               ▼
              Patch A          Patch B
                 │               │
                 └───────┬───────┘
                         ▼
                   Evaluation
                         │
           ┌─────────────┼─────────────┐
           ▼             ▼             ▼
         tests       benchmarks     quality
           │             │             │
           └─────────────┼─────────────┘
                         ▼
                   Comparative Result
                         │
                         ▼
                  Experience Ledger
```

---

# 7. First Concrete Success Criterion

Forge V0/V1 should be able to:

1. accept a Git repository,
2. accept a structured engineering task,
3. create isolated copies/worktrees from the same base commit,
4. run Claude Code in one workspace,
5. run Codex in another workspace,
6. capture their execution,
7. collect their resulting patches,
8. independently run tests,
9. independently run benchmarks,
10. store all raw metrics,
11. compare the outcomes,
12. persist the entire experiment,
13. report which result was superior and why.

This is the first point where Forge becomes genuinely interesting.

---

# 8. Recommended Technology Stack

| Layer | Technology |
|---|---|
| Core implementation | Rust |
| Async runtime | Tokio |
| CLI | clap |
| Serialization | serde |
| Initial database | SQLite |
| DB access | sqlx |
| Later relational DB | PostgreSQL |
| Git integration | Git CLI initially |
| Isolation V0 | Git worktrees |
| Isolation later | Docker |
| API later | Axum |
| Web UI later | React / TypeScript |
| Observability later | OpenTelemetry |
| Learned router | simple statistical model first |
| Graph database | None initially |

Important rule:

> **Do not introduce infrastructure before it is necessary.**

Start local. Start monolithic. Start CLI-first.

No Kubernetes for V0.

No distributed deployment for V0.

No graph database for V0.

No microservices for V0.

---

# 9. Proposed Repository Layout

```text
forge/
│
├── crates/
│   │
│   ├── forge-core/
│   │   ├── task.rs
│   │   ├── run.rs
│   │   ├── result.rs
│   │   └── events.rs
│   │
│   ├── forge-agent/
│   │   ├── adapter.rs
│   │   ├── claude.rs
│   │   ├── codex.rs
│   │   └── pi.rs
│   │
│   ├── forge-executor/
│   │   ├── process.rs
│   │   ├── workspace.rs
│   │   └── sandbox.rs
│   │
│   ├── forge-git/
│   │   ├── worktree.rs
│   │   ├── diff.rs
│   │   └── repository.rs
│   │
│   ├── forge-eval/
│   │   ├── tests.rs
│   │   ├── benchmark.rs
│   │   ├── lint.rs
│   │   ├── complexity.rs
│   │   └── evaluator.rs
│   │
│   ├── forge-store/
│   │   ├── sqlite.rs
│   │   ├── migrations.rs
│   │   └── queries.rs
│   │
│   ├── forge-router/
│   │   └── router.rs
│   │
│   └── forge-cli/
│       └── main.rs
│
├── schemas/
│
├── fixtures/
│   └── test-repositories/
│
├── benchmarks/
│
└── docs/
```

Keep these boundaries logical even if the initial implementation is smaller.

---

# 10. Core Domain Abstractions

Forge should establish stable abstractions early.

## EngineeringTask

Represents a structured task assigned to an agent.

Example:

```json
{
  "task_id": "T-1042",
  "repository": "distributed-runtime",
  "objective": "Improve checkpoint write throughput",
  "constraints": [
    "All existing tests must pass",
    "Recovery semantics cannot change",
    "Memory increase must remain below 10%"
  ],
  "evaluation": {
    "test_command": "cargo test --workspace",
    "benchmark_command": "./bench/checkpoint.sh"
  }
}
```

Tasks should combine:

- a natural-language objective,
- machine-readable constraints,
- machine-readable evaluation instructions.

---

## AgentAdapter

Forge must not depend directly on a specific coding agent implementation.

Conceptually:

```rust
trait AgentAdapter {
    async fn prepare(&self, ctx: RunContext) -> Result<()>;

    async fn execute(
        &self,
        task: EngineeringTask,
        workspace: Workspace,
    ) -> Result<AgentRun>;

    async fn cancel(&self, run_id: RunId) -> Result<()>;
}
```

Forge should see an agent as:

```text
Agent
 ├── identifier
 ├── harness
 ├── model
 ├── capabilities
 ├── tools
 ├── configuration
 └── execute()
```

Possible implementations:

- Claude Code adapter
- Codex adapter
- Pi adapter
- local model adapter
- future specialized agents

Core principle:

> **Agents are interchangeable engineering workers.**

---

## Workspace

Represents an isolated repository environment.

Initial implementation:

- Git worktree
- unique branch
- known base commit
- dedicated working directory

Later:

- container
- microVM
- remote worker

---

## AgentRun

Represents one attempt by one agent on one task.

Contains:

- run ID,
- task ID,
- agent configuration,
- base commit,
- start time,
- finish time,
- exit status,
- stdout/stderr references,
- generated patch,
- event stream,
- token/cost information if available,
- resource usage if available.

---

## Evaluation

Represents Forge's independent judgment of a resulting change.

Must remain separate from the agent's own self-assessment.

---

# 11. Agent Isolation Strategy

Every competing agent should begin from exactly the same repository state.

Example:

```text
repo/
forge-worktrees/
    task-1042-claude/
    task-1042-codex/
```

Both worktrees start from the same commit.

```text
Claude Code → branch A
Codex       → branch B
```

Advantages:

- no cross-agent contamination,
- no concurrent file collisions,
- straightforward Git diffs,
- deterministic comparison,
- simple cleanup.

Git worktrees solve enough of the early isolation problem that Forge should use them before introducing containers.

---

# 12. Event-Sourced Execution History

Forge should record every agent run as a structured event stream.

Do not store only:

```text
task → success
```

Store the trajectory.

Possible event types:

```text
RunStarted
WorkspaceCreated
AgentStarted
PromptSubmitted
FileRead
FileModified
CommandExecuted
TestFailed
TestPassed
BenchmarkStarted
BenchmarkCompleted
AgentFinished
EvaluationStarted
EvaluationCompleted
RunScored
RunFailed
RunCancelled
```

Example:

```json
{
  "run_id": "R-8821",
  "timestamp": "2026-08-10T21:32:15Z",
  "event_type": "CommandExecuted",
  "data": {
    "command": "cargo test -p storage",
    "exit_code": 1,
    "duration_ms": 4821
  }
}
```

This event stream is important because it becomes the raw dataset for future learning.

Forge should preserve:

- commands,
- timings,
- failures,
- retries,
- file changes,
- test results,
- evaluation results,
- agent configuration.

---

# 13. Independent Evaluation

A fundamental Forge principle:

> **Never trust the coding agent to decide whether its own work succeeded.**

Execution boundary:

```text
Coding agent
     │
     ▼
Produces change

──────── TRUST BOUNDARY ────────

Forge evaluator
     │
     ├── compile
     ├── test
     ├── benchmark
     ├── lint
     ├── security checks
     ├── complexity analysis
     └── architectural constraints
```

Deterministic evaluation should be preferred wherever possible.

LLM-based review can be added later as an additional signal, not the primary truth source.

---

# 14. Evaluation Dimensions

Forge should avoid reducing every run immediately to one scalar score.

Store raw measurements and normalized dimensions.

Possible dimensions:

```text
Correctness
Performance
Memory
Maintainability
Security
Change size
Complexity
Build time
Runtime stability
Cost efficiency
```

Example:

```text
Correctness       1.00
Performance       0.91
Memory            0.83
Maintainability   0.78
Security          1.00
Change size       0.72
Cost efficiency   0.88
```

Raw values should also be retained:

```text
tests passed:       429 / 429
benchmark before:   3.84 GB/s
benchmark after:    4.72 GB/s
memory before:      812 MB
memory after:       844 MB
lines changed:      183
agent tokens:       94,201
runtime:            11m 42s
```

Never discard raw evaluation data.

Weights can evolve later.

---

# 15. Evaluator Plugin System

Forge should eventually expose evaluators behind a common interface.

Initial evaluator types:

```text
TestEvaluator
BenchmarkEvaluator
LintEvaluator
SecurityEvaluator
ComplexityEvaluator
CustomCommandEvaluator
```

Repositories should be able to declare custom evaluation logic.

Example:

```yaml
evaluation:
  tests:
    command: cargo test --workspace

  benchmark:
    command: ./bench/checkpoint.sh

  lint:
    command: cargo clippy --workspace -- -D warnings
```

---

# 16. Experience Ledger

Forge's long-term value comes from retaining structured engineering experience.

Core relationship:

```text
TASK
 ↓
ATTEMPT
 ↓
AGENT
 ↓
CONFIGURATION
 ↓
PATCH
 ↓
OUTCOME
```

Additional relationships:

```text
Agent
  │
performed
  ▼
Run
  │
attempted
  ▼
Task
  │
concerned
  ▼
Repository Area

Run
 │
produced
 ▼
Patch

Patch
 │
caused
 ▼
Metric Change
```

Eventually Forge may infer relationships such as:

```text
Task Type ───── successful_with ──── Agent

Agent ───────── strong_at ────────── Rust debugging

Technique ───── improved ─────────── Throughput

Patch ───────── caused ───────────── Regression
```

This is conceptually a graph, but a graph database is unnecessary at first.

---

# 17. Initial Database Schema

SQLite is sufficient initially.

Likely tables:

```text
repositories
tasks
agents
agent_configs
runs
events
patches
evaluations
metrics
artifacts
commits
```

Relationship structure:

```text
repository
   │
   └── tasks
         │
         └── runs
              │
              ├── agent_config
              ├── events
              ├── patch
              └── evaluation
                     │
                     └── metrics
```

Potential migration path:

```text
SQLite
  ↓
PostgreSQL
  ↓
analytical or graph-specific stores only if justified
```

---

# 18. CLI-First Product Surface

Initial CLI:

```bash
forge init
forge agent list
forge run task.yaml --agent claude
forge run task.yaml --agent codex
```

Then:

```bash
forge compete task.yaml --agents claude,codex
```

Later:

```bash
forge history
forge agent stats codex
forge task similar T-1042
forge failures --component storage
forge run task.yaml --agent auto
forge team task.yaml
```

The CLI should remain useful even after a web interface exists.

---

# 19. First Competitive Experiment

Once basic execution works, create approximately 20 real engineering tasks against one or more repositories.

Run:

```text
Claude Code
Codex
```

independently against every task.

Measure:

```text
overall success rate
median runtime
median cost
test regressions
average patch size
benchmark wins
memory regressions
compile failures
number of retries
```

Then classify performance by task type.

Example:

```text
Rust debugging
Claude: 72%
Codex: 91%

Architecture modification
Claude: 89%
Codex: 77%

Test generation
Claude: 94%
Codex: 93%
```

This is where Forge begins generating original empirical evidence.

---

# 20. Learned Routing

After enough historical runs, Forge should support:

```bash
forge run task.yaml --agent auto
```

Routing inputs may include:

- language,
- repository subsystem,
- task category,
- complexity,
- expected patch size,
- available tools,
- agent cost,
- historical success,
- historical regression rate,
- similar tasks.

Conceptually:

```text
Task
 │
 ▼
feature extraction
 │
 ├── language
 ├── subsystem
 ├── task type
 ├── complexity
 ├── expected change size
 └── historical analogues
 │
 ▼
routing model
 │
 ▼
agent configuration
```

The first routing model should be simple.

Good starting choices:

- heuristic scoring,
- logistic regression,
- gradient-boosted trees later if needed.

Do **not** immediately use another LLM for routing.

Forge should output its reasoning:

```text
Selected: Codex

Similar historical tasks: 17
Codex success: 88%
Claude success: 71%

Predicted success:
Codex 0.86
Claude 0.69
```

---

# 21. Configuration Optimization

Forge should eventually choose more than the model.

A run configuration may include:

```text
model
harness
tools
context strategy
review policy
timeout
resource budget
sandbox policy
```

Example learned configuration:

```text
Task:
distributed Rust deadlock

Preferred configuration:

Model:
strong reasoning model

Harness:
Pi

Tools:
cargo
rr
eBPF
flamegraph

Context:
architecture summary
relevant source files
previous failed approaches

Reviewer:
separate review agent
```

Forge is then optimizing an **engineering computation configuration**, not merely selecting a model.

---

# 22. Context Optimization

Agent performance depends heavily on context.

Forge should eventually measure which context helps which tasks.

Possible context sources:

```text
README
architecture documents
relevant code files
recent commits
related issues
previous failed attempts
repository invariants
historical agent trajectories
```

Forge can learn:

```text
Task
 ↓
context selector
 ↓
minimal useful context
 ↓
Agent
```

Measure context strategy against:

- correctness,
- cost,
- latency,
- regression rate,
- patch quality.

This turns context engineering into an empirical optimization problem.

---

# 23. Multi-Agent Execution Comes Later

Do not add multi-agent teams until single-agent evaluation and routing are reliable.

Eventually:

```bash
forge team task.yaml
```

Forge may decide a task requires decomposition.

Example:

```text
                  Forge
                    │
             decompose task
                    │
        ┌───────────┼───────────┐
        ▼           ▼           ▼
    Architect    Research     Profiler
        │           │           │
        └───────────┼───────────┘
                    ▼
                Developer
                    │
                    ▼
                  Review
```

Task dependencies form a DAG:

```text
A ─────► C
 \
  ─────► B ─────► D
```

Nodes are tasks.

Edges are dependencies.

A critical research goal is to compare:

```text
single-agent outcome
vs.
multi-agent outcome
```

Forge should never assume that more agents are automatically better.

---

# 24. Repository World Model

Later Forge should maintain a continuously updated machine-readable model of the repository.

Potential modeled entities:

```text
components
modules
interfaces
contracts
invariants
dependencies
ownership
performance constraints
historical decisions
known failure modes
```

This enables richer reasoning than raw code retrieval alone.

---

# 25. Semantic Conflict Detection

Traditional Git detects textual conflicts.

Forge should eventually detect architectural or semantic conflicts.

Example:

```text
Agent A:
changes Storage trait durability semantics.

Agent B:
changes checkpoint implementation assuming old durability behavior.

Git:
merge succeeds.

Forge:
semantic conflict suspected.
```

Forge can flag:

```text
A changed invariant X.
B appears to depend on invariant X remaining unchanged.
```

This may become a substantial research project by itself.

---

# 26. Longitudinal Repository Health

Forge should eventually evaluate repository evolution, not only individual patches.

Possible longitudinal metrics:

```text
test reliability
cyclomatic complexity
dependency growth
build time
runtime performance
memory use
security findings
duplication
API stability
incident frequency
agent-created regressions
```

Forge should detect situations such as:

> The agents successfully completed 42 consecutive tickets, but maintainability and build performance have degraded for three months.

This distinction is central to Forge's mission.

---

# 27. Security Model

Coding agents execute shell commands, so Forge needs security boundaries from the beginning.

Desired controls:

```text
workspace isolation
command logging
timeouts
process cleanup
CPU limits
memory limits
disk limits
network policy
secret filtering
environment isolation
```

V0:

- local process execution,
- Git worktree isolation,
- conservative environment handling.

Later:

- Docker sandbox,
- stronger container policies.

Eventually:

- microVM or remote worker isolation where appropriate.

Agents should never operate directly against the user's primary working tree.

---

# 28. Observability

Each agent run should eventually behave like a distributed trace.

Example:

```text
Run R-8238

00:00 task started
00:03 agent inspected Cargo.toml
00:18 read storage.rs
00:41 ran cargo test
01:12 test failure
01:34 modified storage.rs
02:01 tests passed
02:47 benchmark regression
03:16 modified allocator
04:29 benchmark +14%
04:37 run finished
```

Long-term concept:

> **OpenTelemetry for coding agents.**

Important observability data:

- timeline,
- commands,
- file reads,
- file changes,
- retries,
- failures,
- evaluation checkpoints,
- resource usage,
- cost,
- token consumption,
- branch/worktree metadata.

---

# 29. UI Comes After the Engine

Do not build the dashboard first.

Eventually a UI may show:

```text
┌───────────────────────────────────────────┐
│ FORGE                              ● LIVE │
├───────────────────────────────────────────┤
│ Repository: distributed-runtime           │
│                                           │
│ Agent runs today                37         │
│ Success rate                   86%         │
│ Cost                         $14.28        │
│ Regressions                      2         │
│                                           │
│ Agent Performance                         │
│ ─────────────────────────────────────     │
│ Codex       ███████████████  91%          │
│ Claude      █████████████    84%          │
│ Pi/local    ███████████      72%          │
│                                           │
│ Recent Runs                               │
│ ✓ Storage contention          Codex       │
│ ✓ API refactor                Claude      │
│ ✗ Memory regression           Codex       │
└───────────────────────────────────────────┘
```

But the CLI and execution engine must exist first.

---

# 30. Development Roadmap

## Phase 0 — Foundation

Implement:

```bash
forge init
forge agent list
forge run task.yaml --agent claude
```

Core requirements:

- Rust workspace,
- config loading,
- EngineeringTask schema,
- AgentAdapter trait,
- AgentRun,
- Event,
- Workspace,
- Evaluation,
- Git worktree creation,
- one real Claude Code adapter,
- command execution,
- event persistence,
- resulting Git diff.

Success condition:

> One task, one real agent, one isolated workspace, one fully observable run.

---

## Phase 0.5 — Evaluation Hardening

Before comparing agents, make Forge's evaluation labels resistant to test
manipulation and patch pollution.

Requirements:

- configurable protected evaluation paths with narrow task-scoped exceptions,
- integrity comparison against the recorded base commit,
- explicit `WorkspaceDelta -> PatchPolicy -> CandidatePatch` boundary,
- structured and persisted integrity/patch warnings,
- explicit distinction between Git worktree isolation and host containment,
- typed benchmark metrics file with maximize/minimize direction,
- `PASS` only when a candidate exists, checks pass, and integrity is acceptable,
- adversarial deleted/rewritten-test coverage.

Success condition:

> Deleting or weakening a failing protected test may make checks green, but can
> never make Forge report `PASS`.

Stop for architectural review after this phase. Do not begin competitive
execution until the trust model is accepted.

---

## Phase 1 — Competitive Execution

Add Codex.

Implement:

```bash
forge compete task.yaml --agents claude,codex
```

Requirements:

- identical starting commit,
- separate worktrees,
- concurrent or sequential execution,
- resulting patch collection,
- independent evaluation,
- comparison report.

Example:

```text
Winner: Codex

Correctness        tie
Performance        Codex +11%
Memory             Claude +2%
Patch complexity   Claude
Cost               Codex
```

This is the Forge MVP.

---

## Phase 2 — Evaluation Framework

Implement evaluator plugins:

```text
TestEvaluator
BenchmarkEvaluator
LintEvaluator
SecurityEvaluator
ComplexityEvaluator
CustomEvaluator
```

Support repository-specific evaluator configuration.

---

## Phase 3 — Experience Ledger

Persist all execution history.

Add:

```bash
forge history
forge agent stats codex
forge task similar T-1042
forge failures --component storage
```

Forge now has institutional memory.

---

## Phase 4 — Learned Routing

Implement:

```bash
forge run task.yaml --agent auto
```

Use historical data to predict the best agent/configuration.

Initially use simple statistical or heuristic models.

---

## Phase 5 — Multi-Agent Execution

Implement:

```bash
forge team task.yaml
```

Features:

- task decomposition,
- DAG representation,
- dependency scheduling,
- isolated workspaces,
- result handoff,
- review,
- comparison against single-agent baselines.

---

## Phase 6 — Repository World Model

Continuously model:

- architecture,
- components,
- interfaces,
- invariants,
- dependencies,
- historical decisions.

---

## Phase 7 — Longitudinal Optimization

Evaluate repository trajectory over time.

Question changes from:

> Did this patch pass?

to:

> Did months of agent work improve the repository?

---

## Phase 8 — Self-Optimizing Engineering System

Forge begins updating its own execution strategy.

Conceptually:

```text
observe
 ↓
act
 ↓
measure
 ↓
learn
 ↓
change strategy
 ↺
```

Forge may learn:

```text
Performance-task configuration:
Codex + Pi + benchmark tools

Architecture-task configuration:
Claude + repository world model + reviewer
```

Policies evolve from evidence.

---

# 31. Dogfooding Strategy

Forge should eventually help build Forge.

Once Forge can run agents safely:

```text
Forge v0.1
    │
    ▼
assign Forge issue to multiple agents
    │
    ▼
evaluate competing implementations
    │
    ▼
select best implementation
    │
    ▼
improve Forge
    │
    ▼
Forge v0.2
```

This gives us a real environment instead of relying only on synthetic benchmark tasks.

---

# 32. Immediate Build Order

Do not skip ahead.

Recommended sequence:

### Step 1
Create the Rust workspace.

### Step 2
Define the core domain objects:

- `EngineeringTask`
- `AgentAdapter`
- `AgentRun`
- `Event`
- `Workspace`
- `Evaluation`
- `Metric`

### Step 3
Implement repository initialization.

```bash
forge init
```

### Step 4
Implement Git worktree creation and cleanup.

### Step 5
Implement generic subprocess execution.

### Step 6
Implement the first real agent adapter.

Recommended first adapter:

```text
Claude Code
```

### Step 7
Run one real task through Forge end-to-end.

### Step 8
Persist events and results in SQLite.

### Step 9
Implement independent test execution.

### Step 10
Complete Phase 0.5 evaluation hardening and stop for architectural review.

### Step 11
Implement Codex adapter.

### Step 12
Implement competitive execution.

```bash
forge compete
```

### Step 13
Implement comparison reporting.

At this stage, stop and benchmark the architecture before adding further complexity.

---

# 33. Initial CLI Target

The earliest useful user flow should look like:

```bash
git clone <repo>
cd <repo>

forge init
```

Create:

```text
.forge/
    config.toml
    tasks/
    runs/
```

Then:

```bash
forge run .forge/tasks/fix_storage.yaml --agent claude
```

Output:

```text
Forge run R-0001

Repository:
distributed-runtime

Base commit:
a73cf21

Agent:
Claude Code

Workspace:
.forge/worktrees/R-0001

Status:
complete

Tests:
429 passed
0 failed

Patch:
183 lines changed

Duration:
11m 42s

Evaluation:
PASS
```

Then:

```bash
forge compete .forge/tasks/fix_storage.yaml \
  --agents claude,codex
```

Output:

```text
Forge experiment E-0002

Claude
------
Correctness: PASS
Benchmark: +8.3%
Memory: +1.2%
Duration: 8m 14s

Codex
-----
Correctness: PASS
Benchmark: +12.6%
Memory: +2.4%
Duration: 6m 51s

Result:
Codex produced the stronger performance improvement.
Claude produced the lower-memory implementation.

Recommended winner:
Codex
```

---

# 34. Design Principles

Agents working on this project should preserve these principles.

## 1. Evidence over agent claims

Forge verifies outcomes independently.

## 2. Raw data over premature scoring

Keep underlying measurements.

## 3. Simple infrastructure first

Local machine, SQLite, Git worktrees, CLI.

## 4. Agent-agnostic architecture

No core logic should depend strongly on Claude Code, Codex, or Pi.

## 5. Reproducibility

Every run should record enough information to understand and ideally reproduce it.

## 6. Determinism where possible

Prefer machine-verifiable tests, benchmarks, and static checks over subjective LLM judgments.

## 7. Longitudinal thinking

A patch succeeding now is not equivalent to software improving.

## 8. Measure orchestration

Do not assume multi-agent systems outperform single agents.

## 9. Build Forge with Forge

Dogfood as soon as practical.

## 10. Avoid premature autonomy

Autonomy should emerge after measurement, reliability, and evaluation are trustworthy.

---

# 35. Research Questions Forge Can Eventually Answer

Forge creates an experimental platform for questions such as:

- Which coding agent is best at Rust concurrency bugs?
- Which agent performs best on architecture-heavy refactors?
- Does providing architecture documentation improve task success?
- How much context is too much context?
- When does multi-agent decomposition outperform a single strong agent?
- Which reviewer model best predicts real regressions?
- Which harness/model combinations produce the smallest reliable patches?
- What agent configuration gives the best quality-per-dollar?
- Does repeated agent-generated code degrade architecture over time?
- Can historical engineering experience improve future task routing?
- Can agent execution strategies adapt without benchmark overfitting?
- Can semantic conflicts be detected before merge?
- What metrics best predict long-term repository degradation?

This makes Forge potentially useful as both:

- an engineering tool,
- and a software-engineering research platform.

---

# 36. Longer-Term Conceptual Model

Forge may eventually evolve toward:

```text
Goal
 ↓
Forge
 ↓
classify problem
 ↓
retrieve historical experience
 ↓
choose organization
 ↓
choose agents
 ↓
choose harnesses
 ↓
choose tools
 ↓
choose context
 ↓
execute
 ↓
verify
 ↓
compare
 ↓
store experience
 ↓
update future policy
 ↺
```

At this point Forge becomes a **persistent autonomous engineering control system**.

---

# 37. What Success Looks Like

A mature Forge system should be able to receive an objective such as:

> Improve checkpoint throughput without weakening recovery guarantees.

Then autonomously:

1. inspect repository history,
2. identify relevant subsystems,
3. retrieve similar historical work,
4. select the strongest agent configuration,
5. create isolated workspaces,
6. test competing approaches,
7. benchmark each implementation,
8. reject regressions,
9. select or merge the best result,
10. record what it learned,
11. monitor longitudinal repository effects,
12. improve the policy used for future work.

That is the long-term destination.

But the first milestone remains intentionally small:

> **One task. One repository. One real coding agent. One isolated execution. One independent evaluation. One recorded result.**

Then:

> **Two agents competing on the same task.**

Build from there.

---

# 38. Instructions for Any Coding Agent Starting This Project

If you are an AI coding agent reading this document, follow these rules:

1. **Do not redesign the entire project before implementing Phase 0.**
2. **Do not introduce Kubernetes, distributed services, or a graph database.**
3. **Keep Forge CLI-first.**
4. **Use Rust for the core runtime.**
5. **Use SQLite for initial persistence.**
6. **Use Git worktrees for initial workspace isolation.**
7. **Create agent adapters behind a provider-agnostic interface.**
8. **Keep evaluator logic independent from agent self-reporting.**
9. **Record structured execution events.**
10. **Preserve raw evaluation metrics.**
11. **Write tests for core state transitions and workspace safety.**
12. **Prefer the smallest implementation that proves the current phase.**
13. **Do not add multi-agent orchestration until competitive single-agent execution is stable.**
14. **Treat this document as the current project architecture unless explicitly superseded by a later design decision.**

---

# 39. First Implementation Assignment

The recommended first task for a coding agent is:

> Create the initial Forge Rust workspace and implement the Phase 0 core abstractions and CLI skeleton.

Required output:

```text
forge/
├── Cargo.toml
├── crates/
│   ├── forge-core/
│   ├── forge-agent/
│   ├── forge-executor/
│   ├── forge-git/
│   ├── forge-eval/
│   ├── forge-store/
│   └── forge-cli/
└── README.md
```

Implement minimally functional versions of:

```text
EngineeringTask
AgentAdapter
AgentRun
Event
Workspace
Evaluation
Metric
```

CLI commands:

```bash
forge --help
forge init
forge agent list
```

At this stage:

- no Claude adapter yet,
- no Codex adapter yet,
- no Docker,
- no web UI,
- no multi-agent execution,
- no learned routing.

The purpose is to establish a clean, compiling foundation before connecting external agents.

---

# 40. Project Thesis

Forge exists because autonomous coding creates a new infrastructure problem.

The question is no longer simply:

> Can an AI write code?

The emerging questions are:

```text
Which agent should work?

What context should it receive?

Which tools should it use?

Can the output be trusted?

Did the change actually improve the system?

What should be remembered?

Which approaches failed?

Which agent is strongest for this kind of work?

Did multiple agents outperform one agent?

Did the repository improve over time?

How should future engineering strategy change based on evidence?
```

Forge is intended to become the system that answers those questions.

---

## Historical Recommended Starting Point

**Phase 0.5: Evaluation Hardening**

This was the recommendation when the design plan was written. Phase 0.5 and
the subsequent Phase 0–8 roadmap are now implemented; this section is retained
to explain the sequencing and trust requirements that shaped the product.

Phase 0's vertical slice is complete. Before adding a second agent, require:

```text
protected evaluation inputs
candidate patch policy
explicit security posture
typed benchmark metrics
adversarial evaluation tests
```

Then stop for architectural review.

Then add the second agent.

Then build competitive evaluation.

Do not move further until this loop is reliable.
