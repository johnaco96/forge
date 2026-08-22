# RC4 qualification drills

All drills below were run after the production-boundary changes and before the
RC4 manifest was frozen. A green drill establishes the named failure behavior;
it does not replace a successful live provider probe or a supervised pilot.

| Area | Drill and expected behavior | Result |
| --- | --- | --- |
| OCI boundary | Host-secret, mount, capability, and network adversarial fixture cannot escape the configured boundary | PASS |
| Evaluator credentials | Contained evaluator runs with no provider credential even when the agent command requires one | PASS |
| Evaluator prerequisite | Missing executable is a typed infrastructure failure and cannot become engineering FAIL/PASS | PASS |
| Memory | Container OOM is classified `MemoryLimitExceeded` | PASS |
| Timeout descendants | Timed-out container and all descendants are removed | PASS |
| Native process timeout | TERM is followed by bounded KILL of the process group; partial output remains evidence | PASS |
| Cancellation | Ctrl-C cancels the run, stops later plan steps, and cannot yield PASS | PASS |
| Provider failure | Nonzero/no-change/malformed/partial provider results retain truthful classifications | PASS |
| Credential contamination | Tracked, untracked, ignored, binary, symlink, path-name, and chunk-boundary matches fail before staging/capture and force destruction | PASS |
| Hostile Git | Hooks, filters, external diff/textconv, Git environment overrides, malicious branch names, and redirected worktree metadata are neutralized or rejected | PASS |
| Workspace lifecycle | Uniqueness, retention, cleanup failure, and durable disposition matrices cover PASS/failure/timeout/cancellation | PASS |
| SQLite concurrency | Sidecar locks serialize restore against active stores; backup remains WAL-safe, verified, staged, and no-clobber | PASS |
| Recovery | Disposable active-store backup, verification, staged restore, and logical post-backup isolation | PASS |
| Rollback | v1.0.1 binary plus v1.0.1-generated config reads a verified schema-12 backup without mutating it | PASS |
| Dependency security | Pinned cargo-audit 0.22.2 scanned 247 locked dependencies; inactive RSA exception remains outside the graph | PASS |

The five Docker-ignored tests were each executed explicitly and passed. The
ordinary workspace run left those five marked ignored, as intended, and passed
the other 709 tests. No ignored test remains unexecuted for this candidate.

Exact-image static doctor checks passed for the FD, HTTPX, and Zod profiles,
including all task-declared tool/version prerequisites and the credential-free
evaluator boundary. Invalid disposable keys then proved both wrappers reached
their provider endpoints and produced typed authentication failures. Those are
negative-path connectivity checks only: they do not satisfy the required
successful mutation probe.

## Post-freeze provider evidence

Both exact-image controlled-mutation probes later passed with approved
invocation-scoped credentials. Seven live pilot tasks also passed with clean
integrity and no production-class infrastructure failure; two Zod tasks were
not attempted and are human-waived. See `release-decision.md`. This addendum
does not reinterpret the deterministic drill results above or mark either
waived task PASS.
