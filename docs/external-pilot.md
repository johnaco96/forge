# Supervised external pilot

The pilot has not been executed. Its purpose is to test Forge outside its own
Rust workspace: repository, language, evaluator and configuration portability;
container robustness; recovery; provider failures; upgrades; router shadow
decisions; and configuration drift.

## Repository selection

Recommended candidates, subject to maintainer/pilot-owner approval and a fresh
baseline inspection, are:

- BurntSushi/ripgrep for Rust. Its repository documents cargo test --all and a
  stable compiler baseline; scope pilot tasks to focused crates/features so the
  build stays bounded.
- encode/httpx for Python. Its contributor workflow provides scripts/install,
  scripts/test, scripts/check, and targeted pytest arguments. Its tests start
  local servers, so the pinned container must reserve the documented local
  ports while external network remains disabled.
- colinhacks/zod for TypeScript. Its repository documents a pinned pnpm
  workspace, build, Vitest, and Biome workflows. Freeze the Node and pnpm
  versions from the selected commit.

Reject toy benchmarks, repositories requiring production credentials, tests
that depend on mutable external services, unbounded builds, unclear licensing,
or tasks already attempted by either effective agent configuration. Record the
full baseline commit, toolchain/lockfiles, container image digest, test commands,
and task revision before any run. Two repositories and two ecosystems is the
minimum gate; three is preferred.

## Execution policy

- agent selection: explicit manual;
- router: recommendation/shadow only, persisted before paired ground truth;
- containment: required;
- network: none unless one narrowly reviewed task needs restricted egress;
- merge: human diff review and approval;
- policy auto-promotion: disabled;
- team auto-dispatch: disabled;
- automatic merge: disabled;
- backup: verified before each repository session and before upgrades.

Begin with two low-risk real tasks per repository, then expand to the agreed
run count only if no campaign-blocking control-plane defect appears. A green
evaluation is evidence for review, never merge authority.

## Required drills

Use deterministic fixture commands, not model-quality tasks, to demonstrate:

1. interrupted agent and descendant cleanup;
2. provider command failure;
3. timeout;
4. low-disk preflight and emergency termination;
5. memory limit and typed OOM;
6. container unavailable with no host fallback;
7. network disabled;
8. required credential absent;
9. WAL-active backup and staged restore;
10. older-schema upgrade;
11. effective configuration fingerprint drift;
12. stale model or harness version;
13. failed-workspace cleanup and evidence preservation.

## Acceptance

The pilot passes only when at least two external repositories across at least
two ecosystems complete supervised real tasks; all applicable drills pass;
doctor passes in each production configuration; no host secret or path escape
is observed; backups restore equal history; upgrades and rollback are
rehearsed; and no campaign-blocking Forge defect remains across the agreed run
count. Results, failures, exclusions, and config strata must be published
without deleting inconvenient evidence.
