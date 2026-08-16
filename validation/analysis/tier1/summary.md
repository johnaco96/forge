# Forge Tier 1 post-campaign analysis

Campaign: `forge-v1-2026-08` v2<br>
Campaign specification: `validation-2026-08` / `a111864145fea8ef182f410e15425cf35c0155e1`<br>
Frozen execution baseline: `v1.0.1` / `781b32fab791d1d4f839bfb1e5988f4e56150048`<br>
Master export: `.forge/validation-archive/tier1-master.jsonl`<br>
Master export SHA-256: `b283ef15c92f3c4c54f104900234638c2c46b2919a2f13a14f7435f3b27903b9`<br>
Generated: `2026-08-16T07:27:51.709350Z` by `tier1-post-campaign-v1` (`fb9927de39e30cc53a3a46dad7b6109e77caf95d8a2fcc7f2fb84f75a9a5d021`)

Analysis repository HEAD: `444bf2d04407759a6a97f293bed2ce03cd9269ed` (`main`, derived worktree changes uncommitted as required).

## Completeness and scope

The master contains **40 attempted**, **40 included**, and **20 complete paired** runs: 20 Claude and 20 Codex. All are live, campaign-tagged, terminal, and based on `781b32fab791d1d4f839bfb1e5988f4e56150048`. The 40 individual point exports match the master exactly as a multiset. There are no malformed records, unknown outcomes, duplicate composite evidence keys, missing exports, duplicate exports, or infrastructure-excluded runs. Every participant-local ledger allocated `R-0001`, which is why `run_id` alone is deliberately not used as a global key.

This is descriptive evidence from one Rust-heavy repository, 20 maintainer-selected non-random tasks, specific Claude Code/Codex CLI harness versions, the 2026 campaign window, and frozen Forge v1.0.1. It does not establish universal provider superiority.

## Outcomes

| Agent | PASS | FAIL | INCONCLUSIVE | INFRA_EXCLUDED | Total | PASS rate |
| --- | --- | --- | --- | --- | --- | --- |
| claude | 17 | 0 | 3 | 0 | 20 | 85.0% |
| codex | 17 | 1 | 2 | 0 | 20 | 85.0% |

The simple historical analyzer is reproduced exactly: **17/20 PASS (85.0%) for each agent**. The equal headline rate masks different non-PASS types and tasks.

### By category

| Category | Agent | N | PASS | FAIL | INCONCLUSIVE | PASS rate | Integrity | Median ms | Median tokens | Median patch lines |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| debugging | claude | 4 | 4 | 0 | 0 | 100.0% | 0 | 447,898.50 | 2,857,598 | 185 |
| debugging | codex | 4 | 4 | 0 | 0 | 100.0% | 0 | 434,732 | 2,749,614.50 | 174 |
| feature | claude | 4 | 2 | 0 | 2 | 50.0% | 2 | 423,348 | 5,346,358.50 | 222.50 |
| feature | codex | 4 | 1 | 1 | 2 | 25.0% | 3 | 457,126 | 3,456,134 | 333 |
| refactor | claude | 4 | 3 | 0 | 1 | 75.0% | 1 | 488,978 | 6,116,933.50 | 160 |
| refactor | codex | 4 | 4 | 0 | 0 | 100.0% | 0 | 515,138 | 3,571,200.50 | 420.50 |
| testing | claude | 3 | 3 | 0 | 0 | 100.0% | 0 | 543,609 | 2,410,544 | 225 |
| testing | codex | 3 | 3 | 0 | 0 | 100.0% | 0 | 380,550 | 1,385,782 | 197 |
| performance | claude | 3 | 3 | 0 | 0 | 100.0% | 0 | 344,682 | 1,801,235 | 139 |
| performance | codex | 3 | 3 | 0 | 0 | 100.0% | 0 | 473,957 | 2,479,351 | 134 |
| persistence | claude | 2 | 2 | 0 | 0 | 100.0% | 0 | 376,242.50 | 2,307,101.50 | 73 |
| persistence | codex | 2 | 2 | 0 | 0 | 100.0% | 0 | 358,144.50 | 1,891,785.50 | 67 |

These are campaign-specific cells as small as n=2 or n=3, not model rankings.

## Paired outcomes

- Both PASS: **16**
- Claude PASS / Codex non-PASS: **1**
- Codex PASS / Claude non-PASS: **1**
- Both non-PASS: **2**

Non-PASS subtypes are preserved in `paired-results.csv`: T-VAL-006 is PASS/FAIL, T-VAL-012 is INCONCLUSIVE/PASS, and T-VAL-021/T-VAL-022 are INCONCLUSIVE/INCONCLUSIVE.

## Runtime and patch size

Runtime delta is defined as `Claude - Codex`; positive means Codex was faster. Claude median runtime was **456,343 ms** and Codex median was **452,258.50 ms**. Raw per-task speed: Claude faster on **9**, Codex faster on **11**, exact ties **0**. The preregistered 20% threshold is used only for pair winner classification; no new near-tie threshold was invented.

Claude median patch size was **174.50 lines across 2 files**; Codex median was **202 lines across 2.50 files**. Patch size is descriptive, never a quality tiebreaker.

## Provider-reported tokens and accounting

Claude provider-reported medians: **3,462,993.50 input**, **28,980 output**, **3,489,364 total**. Codex export medians: **3,049,105.50 input**, **15,628 output**, **3,067,987.50 total**. Provider accounting semantics may differ, so those totals are not treated as a direct efficiency contest.

Claude reported **$50.09 known total USD**, median **$2.0095**, mean/cost per attempted run **$2.5046**, and **$2.9465 per trustworthy PASS**. Spending on non-PASS attempts remains in every denominator.

Codex accounting coverage is 20 runs; model 20/20, input/output 20/20, cached input 20/20, derived credits 20/20, provider credits 0/20, billed USD 0/20. All recovered models are `gpt-5.6-sol`. Total derived credits: **1,286.2737**; median: **66.1924**; mean: **64.3137**; credits per trustworthy PASS: **75.6632**; pooled cache-hit ratio: **95.6%**; median per-run ratio: **95.4%**. Codex provider credits, billed USD, and credit-equivalent USD remain unknown—not zero. No dollar-cost winner is claimed.

## Benchmarks

| Task | Metric | Claude | Codex | Abs delta | % vs Claude | Direction winner |
| --- | --- | --- | --- | --- | --- | --- |
| T-VAL-016 | store_suite_wall_ms | 1,176 | 1,056 | 120 | 10.20% | codex |
| T-VAL-017 | store_suite_wall_ms | 1,071 | 1,038 | 33 | 3.08% | codex |
| T-VAL-018 | store_suite_wall_ms | 1,036 | 1,090 | 54 | 5.21% | claude |

Directional wins: Claude **1**, Codex **2**. T-VAL-017's 3.08% difference is visible but below the preregistered 5% pair-decision threshold.

## Integrity

There were **6 integrity-compromised runs** and **5 green-evaluator runs that Forge refused to call PASS**. The task-level evidence notes and clearly labeled post hoc T-VAL-021/T-VAL-022 benchmark-design classifications are in `integrity-review.md`; no historical outcome was changed.

## Retrospective routing

The production v1 similarity weights, Beta(1,1) scoring, 10-total/3-per-agent readiness, 0.05 margin, and `compete_when_uncertain` policy were replayed with `COALESCE(finished_at, created_at) <= earliest pair created_at`. Routing first became evidence-ready at **T-VAL-009**; 6 tasks occurred before readiness and 14 were evidence-ready.

| Selector | Coverage | Accuracy | Selected PASS rate | Routed PASSes | Regret |
| --- | --- | --- | --- | --- | --- |
| forge_router | 0/20 | unknown | unknown | 0 | 0 |
| always_claude | 20/20 | 50.0% | 85.0% | 17 | 1 |
| always_codex | 20/20 | 50.0% | 85.0% | 17 | 1 |
| seeded_random | 20/20 | 30.0% | 85.0% | 17 | 1 |
| best_global_historical | 4/20 | 50.0% | 75.0% | 3 | 1 |
| category_aware_historical | 4/20 | unknown | 50.0% | 2 | 0 |

Accuracy excludes tied pairs; abstention is not scored as wrong. Regret is only a selection of the non-PASS agent where exactly one agent passed. The seeded-random mapping caveat is recorded in `results.json`. Learned routing added value only if it improved accuracy at comparable coverage; on this campaign it **did not demonstrate added selection value because it abstained on every task**.

The counterfactual replay uses paired observed outcomes, not unobserved claims: always-Claude and always-Codex each yield 17 trustworthy PASSes; Forge routing yields 0 PASSes over 0 covered tasks and abstains elsewhere.

## Readiness and recommendation

Context A/B is registered for four tasks (8 additional runs) but has a 7/8 Tier 1 PASS ceiling. Team comparison is registered for five tasks (5 team executions), and the best observed single agent passed all five. Longitudinal health is pending: no baseline snapshot, no accepted campaign candidate sequence, and no three comparable points. Phase 8 has 0 eligible policy-controlled observations and would return `InsufficientEvidence` before health could become `HealthObservationPending`.

Recommendation: **RUN A REDUCED TIER 2** on T-VAL-016 only (6 runs), then consider a separately registered 20-run DeepSeek cohort after evaluator-layout review. Do not repeat T-VAL-021 unchanged.

## Reproducibility and boundaries

Run `validation/scripts/run-tier1-analysis.sh` with the frozen archive and Codex session directory. The artifacts record campaign/tag/baseline, export digest, tool version/base commit/tool digest, generation timestamp, and rate card. Raw archives are never opened for writing. No Tier 1 task, record, evaluation, patch, log, candidate, execution semantic, or outcome is modified; no agent, Tier 2, context, team, policy, DeepSeek, commit, tag, or Phase 9 action is performed.
