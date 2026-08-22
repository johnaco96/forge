# RC4 exact-image evaluator substrate qualification

- Image: `localhost:5000/forge/pilot-runtime@sha256:5624e2d6abe5fb52282963dbd41e1c9e7c1f3a18653bef2726b4c17e42fecde2`
- Image ID: `sha256:44deba43a2ed6c8f3d40d551164c7ccc128ab9b04b5f3fdeea525f86fee8b1c3`
- Platform: `linux/arm64`
- Execution: credential-free, read-only root, private HOME/tmp, no Linux
  capabilities, no-new-privileges, Docker init, baseline clone writable, Git
  metadata read-only, 2 CPUs, 4 GiB memory, and 256 PIDs.
- Model calls: none.

Every distinct frozen evaluator command set was rerun in this exact image
against a disposable clone of its immutable baseline. Nonzero is acceptable
only when it reproduces the intended baseline engineering signal and the
evaluator itself starts and completes normally.

| Command set | Exact result | Qualification |
| --- | --- | --- |
| FD `cargo test --locked` | 268 passed, exit 0 | runnable |
| FD rustfmt + Clippy | exit 0 | runnable |
| FD overflow contract | exit 1; expected baseline overflow panic detected | runnable; baseline task signal |
| HTTPX Trio write-timeout | 1 passed, 1 ResourceWarning failure | runnable; baseline task signal |
| HTTPX response/header link suites | 133 passed, exit 0 | runnable |
| HTTPX authentication suite | 8 passed, exit 0 | runnable |
| HTTPX `scripts/check` | formatting, mypy, and Ruff passed | runnable |
| Zod focused Vitest/type tests | 202 passed, exit 0 | runnable |
| Zod focused Biome check | 2 files clean, exit 0 | runnable |

All three exact-image static doctor profiles also passed repository, version,
store, disk, containment, evaluator prerequisite, credential boundary, Claude
version, and Codex version checks. At the time of this credential-free
substrate qualification, doctor correctly remained not-ready because successful
live provider probes were not yet available.

Both exact-image controlled-mutation probes subsequently passed during RC4
execution. That post-freeze provider evidence and the seven-run/two-waiver
qualification decision are recorded separately in `release-decision.md`; they
do not rewrite this model-free substrate record.
