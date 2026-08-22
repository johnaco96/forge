# v1.1.0-rc operational failure drills

Executed locally on 2026-08-17. Synthetic fixtures were used where a provider
call was unnecessary. Required evidence was retained in the test ledger or
captured test output; disposable workspaces/containers were removed.

| # | Drill | Expected and actual behavior | Typed failure / preservation / cleanup | Recovery | Result |
|---|---|---|---|---|---|
| 1 | Interrupted subprocess | Timeout killed the command and its child process | `TimedOut`; events retained; descendants gone | inspect evidence, retry | PASS |
| 2 | Provider command failure | Non-zero Codex stub remained distinct from Forge evaluation | provider non-zero outcome; run evidence retained; workspace lifecycle completed | diagnose provider, retry | PASS |
| 3 | Timeout | Host and OCI fixtures stopped at their deadlines | `TimedOut`; logs retained; container force-removed | review tail and retry | PASS |
| 4 | Low disk | Absolute/percentage preflight refused launch; active watchdog killed before ENOSPC | `DiskCapacityLow` / `DiskEmergency`; candidate not blamed; evidence retained | free space, verify store, retry | PASS |
| 5 | Memory limit | OCI allocation crossed the cgroup limit | `MemoryLimitExceeded`; sandbox events retained; container removed | raise reviewed limit or reduce task | PASS |
| 6 | Missing runtime | Required containment and harness probe refused an absent runtime | `SandboxUnavailable`; no host fallback or workspace launch | restore Docker, rerun doctor | PASS |
| 7 | Network disabled | Adversarial container could not reach external network | `network=none`; sentinel evidence retained; container removed | select reviewed policy before retry | PASS |
| 8 | Missing credential | All three real pilot doctors refused absent provider variables without printing values | `CredentialUnavailable`; no job launched | inject the one job-scoped key and rerun doctor | PASS |
| 9 | SQLite backup | WAL-active unit fixture and fd pilot store produced verified backups | integrity/schema/run count retained; source untouched | retain backup off-volume | PASS |
| 10 | SQLite restore | Verified backup restored through staging into a separate pilot-like repository | corruption test left active store untouched; staging cleaned | verify then switch store | PASS |
| 11 | Migration upgrade | Fresh schema and representative schema 7 upgraded to schema 12 | integrity and historical readability preserved | restore pre-upgrade backup on failure | PASS |
| 12 | Fingerprint drift | Every behavioral execution setting changed the fingerprint | deterministic distinct fingerprint; evidence retained | treat as a new configuration stratum | PASS |
| 13 | Stale harness | Expected Codex 0.146.0 versus actual 0.147.0 made doctor red | explicit version mismatch; no agent run; profile restored to 0.147.0 | rebuild image or approve/update profile | PASS |
| 14 | Failed-run cleanup | Failed work was captured durably while its disposable worktree was removed | patch/branch retained; teardown idempotent | inspect run ID and retained artifacts | PASS |

Commands covered 15 process tests, 2 capacity tests, 2 missing-runtime tests,
4 backup/restore tests, 3 real Docker adversarial tests, provider/disk CLI
integration tests, fingerprint tests, and workspace teardown tests. The Docker
suite explicitly tested host-secret/path escape, network-off behavior, typed
OOM, timeout, and descendant cleanup.

Measured pilot-store backup: 0.02 seconds. Measured staged restore: 0.02
seconds. The complete checked-in recovery drill took 1.37 seconds; the migration
gate took 2.18 seconds.

Proposed targets — human approval required: RPO is one completed run or 24
hours, whichever is smaller; RTO is 15 minutes. These allow operational margin
well above the measured single-user local restore while remaining testable.
