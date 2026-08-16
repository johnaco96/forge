# Tier 1 post-campaign analysis

This directory contains derived summaries of the immutable private Tier 1
archive. It contains no raw agent output, candidate patch, evaluator log, or
ledger. Formal outcomes remain exactly as exported.

Reproduce from the repository root:

```bash
validation/scripts/run-tier1-analysis.sh \
  .forge/validation-archive \
  /Users/drewcook/.codex/sessions \
  validation/analysis/tier1
```

The first step invokes the offline `forge-accounting` binary over preserved
Codex logs and matching provider rollouts. The second analyzes the frozen master
export and fails closed unless the 40 records, point exports, base commit,
revisions, agent strata, and 20 pairs are complete. No coding agent is invoked.

`summary.md` is the narrative report. `results.json` is the complete
machine-readable result, `paired-results.csv` and `category-results.csv` are
flat review surfaces, `codex-accounting.jsonl` is additive accounting evidence,
and the two review documents keep post hoc annotations separate from formal
statistics.
