# Forge Validation & Dogfooding Program

Forge's implementation roadmap (Phases 0–8) is complete and tagged `v1.0.0`.
The formal campaign does **not** run from that tag — see §Baseline below.
Nothing in this directory adds a phase, a feature, or an abstraction. This is
the apparatus for answering a question the implementation cannot answer about
itself:

> Does Forge's architecture actually improve autonomous software-engineering
> outcomes under real use?

Everything here began as a **question**, not a claim. Tier 1 has since completed
with 20 real paired tasks and 40 live-agent runs; immutable analysis artifacts
are under validation/analysis/tier1/. The original campaign manifest and
pre-registration language are retained as historical protocol, not current
project status.

---

## Ground rules

These are not stylistic preferences. Violating any of them invalidates the
campaign.

1. **No fabricated agent results.** Codex was unavailable when this protocol
   was written. The later Tier 1 archive contains only the real Codex executions
   that subsequently occurred. A missing measurement stays missing.
2. **No synthetic fixtures as empirical evidence.** `fixtures/test-repositories/median`
   and every `execution_provenance: synthetic` run verify Forge's plumbing.
   They are excluded from every agent-comparison statistic. Forge already
   distinguishes these (`ExecutionProvenance`), and the analysis honours it.
3. **No stdout scraping.** Benchmark values come from the declared typed
   metrics file or they do not exist. This is Forge's own contract
   (`BenchmarkMetrics`), and the campaign does not work around it.
4. **Raw evidence is primary.** `forge export --format jsonl` is the substrate.
   Derived rates and medians are recomputable from it and are never the record
   of what happened.
5. **Infrastructure failure is not engineering failure.** A rate limit, a
   provider outage, or a Forge defect is recorded, classified, and excluded
   from success rates — but never deleted.

---

## Current status

| Element | State |
|---|---|
| 20-task corpus defined and schema-valid | ✅ |
| Tasks Claude already attempted, replaced | ✅ 3 retired, 3 fresh |
| Analysis rules pre-registered (before any Codex result) | ✅ |
| Campaign manifest | ✅ |
| Raw results isolated from participant-visible history | ✅ |
| Campaign baseline frozen | ✅ v1.0.1 validation baseline |
| Claude adapter available | ✅ |
| Claude and Codex live execution | ✅ completed for Tier 1 |
| Paired Claude/Codex campaign | ✅ 20 tasks / 40 runs |
| Tier 1 analysis | ✅ immutable artifacts generated |
| Prospective routing holdout | 📝 preregistered, not executed |
| Claude-only dogfooding | see `results/README.md` |

---

## The validation questions

Six questions, each with a decision rule stated in
[`analysis-plan.md`](analysis-plan.md) **before** the data exists.

### A. Agent comparison
- Which agent performs better, by task category?
- Are the differences stable enough to act on, or inside the noise of a
  20-task sample?

### B. Routing
- Does `forge run --agent auto` select the agent that actually won the paired
  competition more often than the analytical baselines (always-Claude,
  always-Codex, seeded coin-flip, best-global-historical)?
- On how many tasks did routing have enough evidence to decide at all?

### C. Context
- Does Phase 6 world-model context change outcomes?
- What does it cost in latency, tokens, and patch size?

### D. Multi-agent execution
- When does `forge team` beat the strongest single-agent result on the same
  task and base commit?
- What resource multiplier does it charge for that?

### E. Longitudinal health
- Does Phase 7 detect repository degradation introduced by work that passed
  every evaluator?

### F. Policy optimization
- Can Phase 8 produce a proposal from real persisted evidence that improves
  measured outcomes without violating a hard constraint?
- Do promotion and rollback behave correctly against real evidence rather than
  store-integration fixtures?

---

## Layout

```
validation/
  README.md            this file — protocol, ground rules, dogfooding
  campaign.yaml        machine-readable manifest (tasks, agents, modes, metrics)
  analysis-plan.md     pre-registered analysis rules — written before results
  taxonomy.md          category / difficulty / component / tag vocabulary
  tasks/               20 real Forge engineering tasks (IDs are not contiguous;
                       retired tasks are recorded in campaign.yaml)
  scripts/
    validate-corpus.sh   schema-validate every task through Forge itself
    campaign-clone.sh    materialize one isolated participant repository
    test-isolation.sh    prove participant isolation (deterministic, no agent)
    run-dogfood.sh       Claude-only single-agent runs (capped)
    run-campaign.sh      paired Claude/Codex campaign runner
    analyze.sh           deterministic analysis over exported JSONL
  results/
    README.md            where results live and how to read them (no run output)
```

Raw run output is **not** here. It lives in `.forge/validation-archive/`, which
is gitignored and therefore never checked out into a campaign worktree. See
[`results/README.md`](results/README.md).

Tasks live here rather than in `.forge/tasks/` deliberately: `.forge/` is
per-repository working state (and `.forge/forge.db` is the ledger), whereas the
corpus is a versioned artifact that must be reviewable in a diff and
reproducible from a tag.

---

## Baseline (historical pre-run text)

The paragraphs below record the state when the protocol was authored. The
campaign later froze and ran from the validation baseline; they are retained to
show that the baseline was not silently selected after outcomes.

The formal campaign has **no baseline yet**, and `campaign.yaml` carries
`baseline_commit: null` with `baseline_frozen: false` so that cannot be
overlooked.

It deliberately will not be `v1.0.0` / `c566354`. That commit contains a
patch-capture defect that made it impossible to record a result for any agent
that compiled the project — two of the three dogfood runs failed exactly that
way, through no fault of the agent. Running a campaign from it would produce
twenty infrastructure errors and no evidence.

The baseline is intended to be a small validation-hardening release (`v1.0.1`).
Once it is reviewed and tagged, fill in `baseline_commit`, `baseline_tag`, and
`base_rev`, set `baseline_frozen: true`, and do not change them again for this
campaign. Both agents must run from that one commit.

---

## Isolation

Git worktrees share one object store and one ref namespace, so an agent working
in a campaign worktree can list and read every other participant's candidate —
by branch name and by raw object id. Nothing in the checkout prevents it, and
refusing to start when *old* candidate branches exist does not help, because the
branch that matters is the one the other participant creates mid-pair.

Formal participants therefore get **independent clones** of the frozen baseline
(`independent-clone-v1`), one per participant per task, with no shared object
database, no shared refs, and no remote. The mechanism and its invariants are in
`campaign.yaml: isolation`; `scripts/test-isolation.sh` proves them
deterministically and also asserts that the worktree approach leaks, so the test
fails if anyone reverts to it.

Maintainer history — including the `forge/R-000N` dogfood candidates — stays in
this repository. The campaign does not depend on deleting it; participants
simply cannot reach it.

---

## Reproducibility controls

Every campaign run pins the following, and the analysis refuses to combine rows
that disagree on any of them without saying so:

| Control | Source of truth |
|---|---|
| Repository | `campaign.yaml: repository` matched against `[repository].name` |
| Baseline commit | `campaign.yaml: baseline_commit`, recorded per run as `base_commit` |
| Task identity | `task_revision_id` — Forge's immutable content hash, not the file path |
| Agent identity | `AgentConfig.config_fingerprint` from the ledger |
| Evaluator plan | Resolved from the task revision before the agent runs |
| Protected paths | `protected_paths:` in the task; violations recorded as integrity failures |
| Execution trust | `ExecutionProvenance` (`live` for campaign runs) |
| Selection reason | `SelectionSource` (`manual` / `automatic` / `competition`) |
| Policy in force | `policy_id`, `policy_fingerprint`, `policy_decision_id` on the run |

**Pairing rule.** A Claude/Codex pair is only a pair when both runs share a
`task_revision_id` **and** a `base_commit`. Forge's `compete` path guarantees
this by resolving the base commit once before any adapter executes
(`Runner::compete`). Sequential runs against a repository that moved in between
are not a pair and are excluded from paired statistics.

---

## Configuration and version drift

A campaign spanning days will see model updates, CLI updates, and configuration
edits. Drift is recorded, never averaged away.

- Every run carries its `AgentConfig` fingerprint in the ledger.
- Before each campaign session, `scripts/run-campaign.sh` records `claude
  --version` / `codex --version` and the config fingerprint into
  `results/<session>/environment.json`.
- If a fingerprint changes mid-campaign, the analysis reports the affected
  tasks as a separate configuration stratum. It does not silently pool them.
- A material harness change (different model family, different sandbox mode)
  invalidates cross-stratum comparison for the affected tasks. Those tasks are
  re-run or reported as not comparable.

---

## Repetition policy

- **Tier 1 (required):** 1 Claude run + 1 Codex run per task, all 20 tasks,
  paired via `forge compete`. This is the roadmap minimum.
- **Tier 2 (optional, budget-gated):** 3 runs per agent on 5 designated
  representative tasks — `T-VAL-001`, `T-VAL-021`, `T-VAL-009`, `T-VAL-013`,
  `T-VAL-016` (one per category cluster). Tier 2 exists to estimate
  within-agent variance so Tier 1 differences can be read against it.

Tier 2 is **not** run automatically. It requires an explicit decision because it
triples campaign spend. Tier 1 results are reported with the explicit caveat
that single-run-per-agent cannot separate agent skill from run-to-run variance.

---

## Dogfooding: running Forge on Forge

The point is to exercise the **control plane**, not to get code written. Use:

```bash
cargo build --release
./target/release/forge run validation/tasks/T-VAL-XXX.yaml --agent claude --keep-workspace
```

Not `claude` editing the repository directly. The value is in what Forge
records: an isolated worktree, an independent evaluation, a persisted
trajectory, and a candidate branch that a human decides about afterwards.

**Nothing merges automatically.** A dogfood run produces a candidate branch
(`forge/<run-id>`) and a Forge report. Accepting it is a separate, explicit
human act — the agent's own claim of success carries no weight, and neither
does a green evaluation. Review the diff.

Claude-only dogfood runs are **real evidence** and are labelled
`tier: dogfood`, `agents: [claude]` in the results schema. They are **not** a
substitute for the paired comparison and are excluded from every
agent-comparison statistic in [`analysis-plan.md`](analysis-plan.md).

### Execution cap

Pre-campaign Claude-through-Forge dogfooding is capped at **5 runs**. The
deliverable of this stage is campaign readiness, not a body of results.

---

## Health checkpoints

Health snapshots are built at defined checkpoints, not after every commit —
a snapshot per trivial change produces a series that measures noise.

| Checkpoint | When |
|---|---|
| `baseline` | Campaign baseline commit, before any task runs |
| `dogfood-5` | After ~5 accepted dogfood changes |
| `campaign-10` | After ~10 accepted campaign changes |
| `campaign-20` | After the campaign concludes |

At each checkpoint, on a clean tree:

```bash
./target/release/forge world build
./target/release/forge health build
```

`forge health build` requires a world model at the exact commit and a clean
working tree. Three or more snapshots are required before
`forge health trend` reports a direction — with fewer, the answer is
`InsufficientData`, and that is the correct answer, not a gap.

---

## Known limitations

Stated up front so no reader has to infer them.

1. **n = 20.** One repository, one language, one campaign window. Descriptive
   statistics only. No claim of general Claude-vs-Codex superiority is
   available from this design, and none will be made.
2. **Single repository.** Forge is a Rust CLI/library workspace. Results do not
   transfer to web frontends, notebooks, or polyglot repositories.
3. **Task authorship bias.** The corpus was authored by inspecting Forge's own
   code. Tasks are real, but they are the tasks *one reviewer noticed*, which is
   not a random sample of engineering work.
4. **Cost data is partial.** Cost is recorded only when a provider reports it.
   `None` means unknown and is never treated as zero — Forge's own convention.
5. **Health needs time.** Longitudinal dimensions cannot be concluded inside one
   campaign. `HealthObservationPending` is the honest answer early on, and the
   policy optimizer is built to return it rather than guess.
6. **Routing evaluation is retrospective.** It reconstructs what routing would
   have decided at a historical cutoff. It is only as good as the cutoff
   discipline, which the analysis plan defines precisely.
7. **Tier 1 confounds skill with variance.** One run per agent per task cannot
   separate them. Tier 2 exists for this and may not be funded.
