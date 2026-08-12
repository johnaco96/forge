# Validation and dogfooding

Phases 0–8 are complete and tagged `v1.0.0`. The implementation roadmap answered
*can Forge do this?* It could not answer the question the project exists for:

> Does any of this actually improve autonomous software-engineering outcomes?

That question is empirical, and the apparatus for answering it lives in
[`validation/`](../validation/).

---

## Why this is separate from the phases

Every phase added a capability and proved it worked with tests. Tests establish
that the mechanism behaves as specified. They cannot establish that the
mechanism helps, because the thing being claimed — better engineering outcomes —
is not observable from inside a unit test. Only real agents doing real work on a
real repository can produce that evidence.

So validation is deliberately **not** Phase 9. It adds no abstraction, no
router, no optimizer, no schema. It is a corpus, a protocol, a manifest, and
four shell scripts around the existing CLI.

---

## What is measured

Six questions, stated as questions:

| # | Question | Mechanism under test |
|---|---|---|
| A | Which agent is better, by task type? | `forge compete` |
| B | Does learned routing beat naive baselines? | Phase 4 routing, evaluated retrospectively |
| C | Does world-model context help or cost? | Phase 6 context |
| D | When does a team beat the best single agent? | Phase 5 team execution |
| E | Does health detect degradation that passed evaluation? | Phase 7 health |
| F | Can policy optimization improve outcomes safely? | Phase 8 policy |

Decision rules for all six are pre-registered in
[`validation/analysis-plan.md`](../validation/analysis-plan.md), written before
any result existed. That document is the campaign's protection against the most
likely failure mode of a self-evaluating system: choosing the comparison after
seeing which comparison flatters it.

---

## The corpus

Twenty real Forge engineering tasks, in
[`validation/tasks/`](../validation/tasks/), balanced across six categories.
Each was written by inspecting Forge's own source at the baseline commit, and
each describes a condition that is actually there — an N+1 query that exists, a
CLI listing that is genuinely missing, an invariant no test currently pins.

Two rules governed authorship:

1. **The task states the outcome, never the patch.** An agent reading the task
   learns what should be true afterwards and why it matters, not which lines to
   change.
2. **The evaluation is independent and not gameable from inside the task.**
   `cargo test --workspace` and `cargo clippy -D warnings` are required
   everywhere — they are Forge's own release gate. The campaign definition
   itself is listed in `protected_paths`, so an agent cannot alter the thing
   that judges it.

There is one honest limitation here worth stating plainly: Forge's unit tests
live in `#[cfg(test)] mod tests` blocks inside the same source files agents
edit, so they cannot be protected by path. Cross-crate integration tests under
`crates/*/tests/` can be and are. Integrity violations are recorded and reported
separately regardless.

---

## Running it

```bash
cargo build --release --bin forge
validation/scripts/validate-corpus.sh          # schema + campaign taxonomy
validation/scripts/run-campaign.sh --dry-run   # readiness, spends nothing
```

The readiness check refuses to start a campaign that cannot produce the
evidence it claims to — including the case where an agent's adapter is
implemented but its CLI is not installed, which `forge agent list` reports in
two separate columns and which is easy to misread.

Claude-only dogfooding, capped at five runs:

```bash
validation/scripts/run-dogfood.sh T-VAL-021
```

Paired campaign, once both agents are available:

```bash
validation/scripts/run-campaign.sh
validation/scripts/analyze.sh validation/results/campaign-<session>/export.jsonl
```

---

## Isolation: why formal runs use clones, not worktrees

Forge isolates ordinary runs with Git worktrees. That is the right mechanism for
ordinary use — cheap, fast, and enough to keep two runs from treading on each
other's files.

It is not enough for a controlled experiment. Sibling worktrees share one object
database and one ref namespace, so the participant running second can read the
first one's finished candidate:

```bash
git branch -a                  # the other participant's branch is listed
git show forge/R-00NN:file.rs  # and its contents are readable
git cat-file -e <candidate>    # reachable by raw object id too
```

The paired comparison depends on neither agent having seen the other's work, so
the formal campaign gives each participant its own repository, cloned from the
same frozen baseline:

```text
frozen campaign baseline
        ├── independent clone → Claude
        └── independent clone → Codex
```

Both solve the identical task from the identical commit; only the physical
repository differs. Results are paired on recorded metadata —
`(campaign_id, task_revision_id, base_commit)` — never on having shared a parent
repository.

One detail is load-bearing and easy to get wrong: a **local** `git clone` does
not negotiate a pack, it copies the entire object directory. `--single-branch`
then filters the refs and leaves every object behind, which looks isolated and
is not — the other participant's candidate stays reachable by id. The campaign
clones with `--no-local`, which forces the transport path so only objects
reachable from the requested ref are transferred, and removes the remote
afterwards so nothing withheld can be fetched later.

`validation/scripts/test-isolation.sh` proves this deterministically, with no
agent and no credits. It also asserts the *old* mechanism leaks, so the test
would fail if anyone reverted to it.

**Scope.** This is campaign infrastructure, not a change to how Forge executes
anything. Forge's worktrees still share an object store; the claim here is that
the formal experiment is protected from cross-participant leakage, not that
worktrees have become hardened against a hostile participant.

---

## Rules that are not negotiable

- **No fabricated results.** Codex is unavailable; no Codex measurement exists
  anywhere in this repository.
- **No synthetic evidence in empirical claims.** `execution_provenance` already
  separates them, and the analysis honours it.
- **Raw evidence is primary.** Every statistic is recomputable from
  `forge export --format jsonl`.
- **Infrastructure failure is not engineering failure**, and is neither counted
  as one nor deleted.
- **Nothing merges automatically.** A green evaluation is evidence, not a
  decision.

---

## Status

Claude-only dogfooding has run and is complete; it exercised the control plane
and surfaced five real Forge defects, all fixed. The three tasks Claude
attempted were retired from the corpus and replaced, so no formal task carries
prior exposure for either agent.

The paired Claude/Codex campaign has **not** been executed, and no part of this
repository claims otherwise. It is fail-closed on three independent conditions —
Codex's CLI is absent, the campaign baseline is not yet frozen, and candidate
branches from dogfooding are still present — and `run-campaign.sh --dry-run`
reports each of them.

The baseline will be a small validation-hardening release rather than `v1.0.0`:
that commit could not capture a patch from any agent that compiled the project,
so a campaign run from it would have produced twenty infrastructure errors.

See [`validation/results/`](../validation/results/README.md) for what has
actually run and where the raw evidence lives.
