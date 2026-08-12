# Validation results

This directory is **public campaign material**. It describes how results are
produced, stored, and read. It deliberately contains no run output.

---

## Where results actually live

Raw campaign and dogfood artifacts are written outside version control:

```
.forge/validation-archive/          gitignored; never checked out into a worktree
  <session>/
    environment.json                versions, config fingerprints, base commit
    <task-id>.report.txt            Forge's own report, verbatim
    exit-codes.txt
    export.jsonl                    point-in-time ledger export
  findings.md                       maintainer defect analysis
```

Two reasons, and both matter more than tidiness:

1. **Campaign worktrees are checkouts of the baseline commit.** Anything not
   committed cannot appear in one. Keeping raw results out of Git is what makes
   "a participant cannot read prior results" a structural property rather than a
   convention.
2. Run reports and exports quote task objectives, candidate diffs, evaluator
   output, and root-cause analysis. For a corpus a future agent will be asked to
   work on, that is the answer key.

The ledger at `.forge/forge.db` remains the system of record. Exports are a
point-in-time capture of it and can be regenerated at any time with
`forge export`.

---

## Provenance labels

| Label | Meaning | Counts toward agent comparison? |
|---|---|---|
| `dogfood` | Claude-only, single agent, pre-campaign | **No** |
| `tier-1-paired` | Both agents, one base commit | **Yes** |
| `tier-2-repetition` | 3 runs/agent on 5 tasks | Yes, as a variance stratum |
| `context-ab` | World-model context A/B | Only within that experiment |
| `team` | `forge team` vs best single agent | Only within that experiment |

---

## What has run so far

Three Claude-only dogfood runs on 2026-08-12, from base `c566354`. They
exercised the control plane and are **not** agent-comparison evidence;
[`../analysis-plan.md`](../analysis-plan.md) §3 excludes them from every paired
statistic.

| Run | Forge outcome | Note |
|---|---|---|
| `R-0001` | ERROR (infrastructure) | Agent completed; Forge could not capture the patch |
| `R-0002` | ERROR (infrastructure) | Same cause |
| `R-0003` | **PASS** | First full pipeline success, after the cause was fixed |

The two errors were **infrastructure failures, not engineering failures**. Per
the analysis plan they are excluded from success rates and reported as
operational reliability. Their verdicts are historical and are never revised.

**No Codex results exist.** Codex's CLI is not on PATH. No row, file, or number
anywhere in this repository describes a Codex run.

Five real Forge defects came out of those three runs. The public summary is in
[`../../docs/validation.md`](../../docs/validation.md); the full root-cause
analysis is maintainer material and lives in the archive.

---

## Reading a session

1. `environment.json` — which versions produced it. A differing
   `config_fingerprint` means a separate stratum, never pooled
   ([`../analysis-plan.md`](../analysis-plan.md) §6).
2. `<task-id>.report.txt` — Forge's independent report. The agent's own account
   of its work appears nowhere in it, by design.
3. `export.jsonl` — the analysis substrate.
4. `../scripts/analyze.sh <export.jsonl>` — descriptive statistics per the plan.

---

## Failure handling

Awkward results are kept, never deleted.

- **Outage, rate limit, timeout, adapter crash** → `RunOutcome::Errored`.
  Excluded from success rates, reported as operational reliability.
- **Agent produced nothing** → `NoChange`. An engineering result; counts as
  non-PASS.
- **Nothing measurable** → `Inconclusive`. Counts as non-PASS.
- **Integrity violation** → `Inconclusive` *and* counted separately.
- **Forge defect** → run kept, defect filed, affected runs listed in the report's
  deviations section.

A session that went badly stays in the archive. Deleting it would make the
campaign look better than it was.

---

## Merge policy

**Nothing merges automatically.** A run leaves a candidate branch
(`forge/<run-id>`) and, with `--keep-workspace`, an inspectable worktree. A
green evaluation is evidence that the declared checks passed — not a decision to
accept the change. Read the diff.

This matters most when the evaluation is green, which is exactly when the review
is most tempting to skip.
