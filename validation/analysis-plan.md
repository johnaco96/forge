# Pre-registered analysis plan — campaign `forge-v1-2026-08`

**Status: registered 2026-08-12, before any campaign run existed and before
Codex was available.**

**Amendment log.** Every change made after registration, with the reason and
whether any formal result existed at the time.

| Date | Change | Reason | Formal results existing |
|---|---|---|---|
| 2026-08-12 | `T-VAL-005` → `T-VAL-021`, `T-VAL-007` → `T-VAL-022`, `T-VAL-011` → `T-VAL-023` in §10 team subset and the Tier 2 list | Claude attempted the first three during dogfooding, so a paired run would not have been a clean first exposure for both agents | **None.** No Codex run exists; no paired result exists |
| 2026-08-12 | Campaign baseline changed from `v1.0.0` / `c566354` to a to-be-frozen hardening release | The old baseline could not record a result for any agent that compiled the project (patch capture failed with `E2BIG`) | **None** |
| 2026-08-12 | Formal execution moved from `forge compete` in one repository to one independent clone per participant (`independent-clone-v1`) | Sibling worktrees share an object database and ref namespace; the participant running second could read the first's candidate by branch name **and** by raw object id, which was reproduced before the change | **None** |

No analysis **rule** was changed: categories, comparison and tie rules,
exclusion rules, routing definitions, thresholds, regret, the context and team
designs, health checkpoints, and the repetition policy all stand exactly as
registered. The amendments above change *which tasks*, *which commit*, and
*where a participant runs* — not how anything is judged. In particular the
pairing rule in §3 is textually the same requirement it was at registration;
only the mechanism that guarantees it changed.

The point of writing this down first is narrow and specific: once results
exist, every one of these choices becomes arguable in a direction that flatters
whichever conclusion is emerging. Fixing them in advance costs nothing now and
removes that freedom later.

Any deviation made after data collection begins must be recorded in
[`results/README.md`](results/README.md) with the reason and the date. Deviating
is allowed; deviating silently is not.

---

## 0. Unit of analysis

The unit is a **task-agent-configuration attempt**: one `(task_revision_id,
agent_id, config_fingerprint, base_commit)` tuple, corresponding to exactly one
`run_id` in the ledger.

Every statistic is computed from `forge export --format jsonl`. No number in any
report may originate anywhere else.

---

## 1. Inclusion and exclusion

### Included
- `execution_provenance == "live"`
- `repository == "forge"`
- `task_revision_id` present in `campaign.yaml: tasks`
- Run reached a terminal `RunStatus`

### Excluded, with the exclusion recorded and counted
| Rule | Reason |
|---|---|
| `execution_provenance != "live"` | Synthetic and imported runs verify plumbing, not agents |
| `outcome == "errored"` | Forge could not carry the run through — infrastructure, not engineering |
| Task not in the manifest | Not campaign evidence |
| `base_commit` differs within a pair | Not a paired comparison (§3) |
| `config_fingerprint` outside the declared stratum | Configuration drift (§6) |

**Exclusions are reported, never dropped silently.** Every report states
`n_attempted`, `n_included`, and a breakdown of exclusions by rule. A campaign
where half the runs errored is a finding about Forge, and it must be visible.

### Infrastructure failure is not engineering failure
`RunOutcome::Errored` means Forge did not complete the pipeline — adapter crash,
timeout, rate limit, provider outage. These are excluded from success rates and
reported separately as an **operational reliability** table: count by agent, by
cause, and by campaign day. They are never counted as agent failures, and they
are never deleted.

---

## 2. Primary outcome: task success

A task attempt is a **PASS** if and only if Forge's own evaluation says so:
`outcome == "passed"`. Forge derives this from required evaluators plus
integrity; the analysis does not re-derive or override it.

Explicitly **not** a PASS:
- `no_change` — the agent changed nothing
- `inconclusive` — nothing measurable, or integrity compromised
- `failed`
- `errored` — excluded entirely (§1)

The agent's own account of what it did is never an input.

**Integrity gate.** Any run with `integrity` not acceptable is recorded as
`inconclusive` regardless of evaluator verdicts, and is additionally counted in
an **integrity violation** table by agent. An agent that edits its own tests has
not succeeded, and this is the number that says so.

---

## 3. Pairing rule

Two runs form a **pair** when they share:
- `task_revision_id` (identical content hash), and
- `base_commit`, and
- a declared configuration stratum (§6)

The rule is unchanged from registration. What enforces it changed once, for
infrastructure reasons recorded in the amendment log: formal runs no longer use
`forge compete`, because running both participants in one repository is exactly
the isolation flaw that made the comparison unsound. Each participant now runs
in its own clone with `--base` pinned to the frozen baseline, which pins the
same two fields `compete` used to pin.

Pairing is therefore on recorded metadata —
`(campaign_id, task_revision_id, base_commit)` plus the configuration stratum —
and never on two runs having shared a parent repository. Sequential runs where
the repository moved in between are **not** a pair and are excluded from all
paired statistics; they may still appear in unpaired per-agent rates, labelled
as such.

---

## 4. Win / loss / tie

Per pair, decided in this order. The first rule that discriminates wins; if none
does, the pair is a tie.

1. **PASS beats non-PASS.** If exactly one side passed, that side wins. No
   secondary metric can overturn this — a faster failure is not a better result.
2. **Both non-PASS → tie**, recorded as `tie-both-failed`. This is deliberately
   not decided on partial credit: neither delivered the outcome.
3. **Both PASS** → compare on the task's declared benchmark metrics, in the
   direction the metric declares (`minimize` / `maximize`). A metric decides
   only if the difference is **≥ 5%** of the baseline value. `neutral` metrics
   never decide.
4. **Both PASS, no deciding benchmark** → compare `agent_runtime_ms`. Decides
   only if the difference is **≥ 20%**. Wall-clock on a shared developer machine
   is noisy; this threshold is deliberately loose.
5. **Otherwise** → `tie-equivalent`.

**Patch size is never a tiebreaker.** Smaller is not better; it is only
different. It is reported as a distribution, not a score.

**The 5% and 20% thresholds are fixed now** and apply symmetrically: a
difference too small to call a win is also too small to call a loss.

---

## 5. Reported statistics

Descriptive only. n = 20 with one run per agent per task does not support
inferential claims, and none will be made.

**Per agent, overall and per `classification.category`:**
- `n`, PASS count, PASS rate
- Runtime: median, min, max
- Tokens: median, min, max — **only over runs where the provider reported them**
- Cost: median and total — **only over runs where cost is known.** `None` is
  unknown, never zero, and never imputed
- Patch size: median files changed, median lines changed
- Integrity violations: count
- Infrastructure failures: count (excluded from rates, reported here)

**Per category:** win / loss / tie counts.

No confidence intervals. With n ≈ 3 per category cell, an interval would span
almost the whole range and would imply a precision the design cannot deliver.
If Tier 2 repetition runs (§ README) are funded, within-agent variance is
reported as an observed range and Tier 1 differences are read against it.

**No composite score.** There is no single "Forge score" and none will be
constructed. A composite would average away exactly the tradeoffs the campaign
exists to observe.

---

## 6. Configuration strata

A **stratum** is a distinct `config_fingerprint` per agent. All statistics are
computed within a stratum.

- If an agent ran the whole campaign under one fingerprint: one stratum, report
  normally.
- If the fingerprint changed mid-campaign: report each stratum separately and
  state which tasks fall in which. Do not pool.
- Cross-stratum pairs are excluded from paired statistics and counted under the
  configuration-drift exclusion.

Model and CLI versions are captured per session in
`results/<session>/environment.json` and reported alongside.

---

## 7. Routing validation (question B)

Retrospective, run **after** the paired campaign and computed strictly from
evidence available at the cutoff.

For each task, in order:
1. Set cutoff `T` = the pair's `base_commit` run creation instant.
2. Reconstruct the routing decision from evidence with
   `COALESCE(finished_at, created_at) <= T`. **The pair's own outcomes are
   after `T` and must not be visible.** This is the single most important
   discipline in the routing analysis; a leak here invalidates the result
   entirely.
3. Record the decision kind: `Selected(agent)`, `InsufficientEvidence`, or
   `CompeteRecommended`.
4. Compare against the pair's actual winner (§4).

**Outcome categories** — every task lands in exactly one:

| Category | Meaning |
|---|---|
| `correct` | Routing selected an agent, and that agent won the pair |
| `incorrect` | Routing selected an agent, and the other agent won |
| `tie-not-scored` | Routing selected, but the pair tied — no ground truth |
| `no-decision` | Routing returned `InsufficientEvidence` or `CompeteRecommended` |
| `not-comparable` | Pair excluded under §1 or §3 |

**Metrics:**
- **Coverage** = `(correct + incorrect + tie-not-scored) / total tasks`. How
  often routing was willing to decide at all.
- **Accuracy** = `correct / (correct + incorrect)`. Ties excluded from the
  denominator — a tie is not a wrong answer.
- **Selected-agent PASS rate** = PASS rate of the runs routing would have
  chosen.
- **Regret**, defined precisely: over tasks where exactly one agent passed and
  routing selected the other, regret = the count of those tasks, and the regret
  rate is that count over `(correct + incorrect)`. Regret is deliberately
  **not** defined on runtime or cost — a slower pass is not a regret worth
  reporting at this sample size.

Forge is expected to report `InsufficientEvidence` on most or all of the first
campaign: its configured thresholds are 10 total and 3 per-agent observations.
**That is the correct behaviour, not a failure**, and a high `no-decision` rate
will be reported as such rather than as a routing defect.

---

## 8. Routing baselines (question B)

Analytical only. None of these is added to Forge as a router.

| Baseline | Definition |
|---|---|
| `always-claude` | Always selects Claude |
| `always-codex` | Always selects Codex |
| `coin-flip` | Deterministic from `SHA256(seed ‖ task_revision_id)`, seed recorded in `campaign.yaml` |
| `best-global-historical` | The agent with the higher PASS rate over all evidence strictly before `T` |

Each is scored with the identical §7 categories and metrics. Learned routing
adds value only if it beats these on accuracy **at comparable coverage** —
a router that decides twice and is right twice has not beaten a baseline that
decided twenty times.

---

## 9. Context experiment (question C)

Subset: the tasks tagged `context-experiment` — `T-VAL-004`, `T-VAL-010`,
`T-VAL-012`, `T-VAL-014`. Chosen because each requires knowing something about
the repository's structure that is not visible in the file being edited: which
evaluator contracts exist, which command modules share a preamble, where the
policy resolver boundary sits, what Phase 0–7 history a ledger actually holds.
If world-model context helps anywhere, it helps here; if it does not help here,
that is informative.

Two arms, agent and configuration held constant, one variable changed:
- **A:** `world_model.enabled = false` — no world-model context
- **B:** `world_model.enabled = true` — Phase 6 task-relevance context

Both arms run from the same `base_commit` and the same `task_revision_id`.

Measured per arm: PASS, runtime, tokens (where reported), patch size, and the
count of world-model facts supplied (`world_model_context` on the run, and
`context_fact_ids` on the policy decision where one exists).

Reported as paired deltas per task. With ~4 tasks per arm this is an
observation, not a result, and will be labelled that way.

---

## 10. Team vs single agent (question D)

Subset: tasks tagged `team-candidate` — `T-VAL-009`, `T-VAL-010`, `T-VAL-012`,
`T-VAL-014`, `T-VAL-016`. These were chosen because each genuinely decomposes
(analyse an invariant → change it → verify), not because a team is expected to
win.

For each: run `forge team` with an explicit validated plan from the same
`base_commit` as the single-agent pair.

Comparison is against the **stronger** of the two single-agent results on that
task, not the mean — the question is whether a team beats the best available
single agent, not whether it beats the average one.

Reported: PASS, benchmark metrics, runtime, tokens, known cost, patch size,
review findings, and a **resource multiplier** = team total tokens ÷ best
single-agent tokens (and the same for runtime and known cost).

A finding that teams cost 3× and pass no more often is a real, publishable
result and will be reported without softening.

---

## 11. Health checkpoints (question E)

Snapshots only at the four checkpoints named in the README. Reported per
dimension at each checkpoint, with the snapshot id.

Phase 7 requires ≥ 3 comparable snapshots before it will report a trend
direction; below that it returns `InsufficientData`. The analysis reports
`InsufficientData` verbatim and does **not** substitute a two-point difference
for a trend. `Changing` is reported as changing, not as degradation.

The specific question — does health detect degradation introduced by work that
passed every evaluator? — can only be answered if such a change actually occurs.
If none does, the honest finding is "not exercised", not "no degradation
detected".

---

## 12. Policy validation (question F)

Sequenced deliberately: Phase 8 does not touch the campaign until the campaign
has produced evidence.

1. Do not enable policy control during Tier 1. Runs are governed by the
   bootstrap policy, which reproduces existing behaviour.
2. After the campaign, run `forge policy propose` against real persisted
   evidence.
3. Record and inspect, without acting: eligible and excluded evidence counts
   with reasons, the cutoff, the objective, every hard-constraint result, the
   recommendation, and the evidence fingerprint.
4. Verify the exclusion reasons are individually correct against the ledger. A
   resolver that excluded the right *number* of runs for the wrong reasons is
   wrong.
5. If a candidate is proposed, prefer shadow, then canary. **A proposal is not a
   reason to promote.** Promotion requires passing the gate and an explicit
   human act, and this campaign will not automate that.

Expected and acceptable outcomes include `InsufficientEvidence` and
`HealthObservationPending`. Both are correct answers from a 20-task campaign and
will be reported as validation *successes* of the optimizer's conservatism, not
as failures to produce a result.

---

## 13. Policy experiment (question F)

One bounded experiment, only once sufficient evidence exists.

Permitted knobs — all bounded parameters that cannot move a trust boundary:
- `context.max_world_facts`
- `routing.minimum_score_margin`
- `resources.timeout_secs`

Explicitly forbidden, and structurally impossible in Phase 8's model: anything
touching required evaluators, the meaning of PASS, protected paths, integrity
rules, provenance, or evidence eligibility. These are `FixedGuardrail`s and
carry no policy field at all.

Design: control = active policy, candidate = one-knob variant, deterministic
assignment by `AssignmentRule`, explicit budget. The same task revision must
always land on the same arm; this is asserted before the experiment starts.

Promotion requires the gate to pass **and** an explicit `forge policy promote`
by a person. Not automated in this campaign.

---

## 14. Reporting rules

Every report states, at the top:
- Campaign id and version
- Repository, baseline commit
- Agent versions and `config_fingerprint`s per stratum
- Date range
- `n_attempted`, `n_included`, exclusions by rule
- Which questions the data **cannot** answer

Claims are scoped to: this task distribution, this repository, these model and
harness versions, this campaign window. No sentence of the form "Claude is
better than Codex" will appear. The supportable form is: "On these 20 Forge
tasks, under these versions, in this window, agent X passed N of 20 and agent Y
passed M of 20."
