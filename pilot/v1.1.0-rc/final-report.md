# Forge v1.1.0-rc qualification report

1. **Starting HEAD:** `be770b858e3aeff90618494b4fc0d133333527ef` on `main`.
2. **Starting tree state:** modified `crates/forge-cli/src/commands/doctor.rs`, `crates/forge-executor/src/lib.rs`, and `crates/forge-executor/src/sandbox.rs`; untracked `dist/`, `docs/releases/`, and `pilot/`. No remote was configured.
3. **Tier 1 SHA verification:** `.forge/validation-archive/tier1-master.jsonl` matched `b283ef15c92f3c4c54f104900234638c2c46b2919a2f13a14f7435f3b27903b9` before live execution.
4. **API credential presence:** `ANTHROPIC_API_KEY = present`; `CODEX_API_KEY = present`. Only non-empty presence was checked.
5. **Credential leak checks:** no value match for either credential across all three pilot `.forge` trees, the retained fd workspace, run artifacts, or ledgers. No value was printed, logged, or copied into the source tree.
6. **Pilot doctors:** fd PASS at `ee20f42`, HTTPX PASS at `b5addb6`, and Zod PASS at `9f0a3d8`; each verified Forge 1.1.0, schema 12, capacity, required containment, digest pin, 2 CPUs, 4 GiB memory, 256 PIDs, 20 GiB workspace ceiling, allowed network policy, one explicit credential, Claude 2.1.223, and Codex 0.147.0. Final doctors remained green; fd then reported one run.
7. **Frozen OCI image:** `localhost:5000/forge/pilot-runtime@sha256:cb33df58846a9f95b46928253ea8aac7d68c31ce18d8fa428e40715b767aac8a` (`linux/arm64`).
8. **Frozen manifest hash:** `5861383dae16e01aea5ae817a8fc2494f753121e76abc59f5edcf5fa476a5ce3`; the offline validator passed all 3 profiles and 9 task hashes.
9. **Repositories:** `sharkdp/fd`, `encode/httpx`, and `colinhacks/zod`.
10. **Exact repository SHAs:** fd `ee20f426ddf338ac7ead5c5f00ea49258005caaf`; HTTPX `b5addb64f0161ff6bfe94c124ef76f6a1fba5254`; Zod `9f0a3d81221e3ab7c09ca4911ef35b54817869a4`.
11. **Ecosystems/languages:** Rust, Python, and TypeScript/Node.
12. **Nine frozen tasks:** FD-001 invalid-UTF-8 formatted output; FD-002 size-unit overflow rejection; FD-003 bounded `--exec-batch`; HTTPX-001 Trio request-stream cleanup; HTTPX-002 quoted Link-header parsing; HTTPX-003 Digest `auth-int`; ZOD-001 `uniqueItems`; ZOD-002 `contains` bounds; ZOD-003 nested local JSON Pointer resolution.
13. **Frozen assignments:** FD-001/002/003 → Claude; HTTPX-001/002/003 → Codex; ZOD-001/002/003 → Claude.
14. **Router shadow results:** all nine decisions were persisted before any outcome and abstained `INSUFFICIENT EVIDENCE` with 0 eligible/resolved runs. FD evidence fingerprints: FD-001 `ab52f8e55bf1724ccd258c2e8e5b79737056ea8cd387ff09c1f55da09d3e0260`, FD-002 `210d92020c2bae22fb6de9bc5232e268f7a3cfa5298adbe48156b6d5e71286dc`, FD-003 `cae44c63269420d73af3f35affba3c766559350c6f85ae725c37df1b5bd34ef8`; HTTPX: `6fefc86d70602dcc46566f8c6db810b40fb9a76437cf2040e3b27200e7ba9e87`, `9a16cf7ab110fc8cb684da6511a439a9d8f049a5be441f6e1912e312d003a762`, `f2ed9d92b8cd6ce3ad1ac9f9bc75a5b10d59087b987107ee778b1bab6aab65b1`; Zod: `d662f467302227a0f45b7bcc56454b24ee279b54f21fb76a9ca27f3ba82ec468`, `7a61110dc970b0be42f077cf9520b1541169a7b6f13d6e1847f02e21be9f7443`, `ebcdf75fde6cb204bbdace43845b34c42cd820d12501f8081e5fbeb72a8ee3be`. Full historical cutoffs are stored in the three ledgers. Effective configuration fingerprints were fd Claude `b341a42dffeadb27`/Codex `8c0c49a28c4ed5f9`; HTTPX Claude `c62905c6bbe362cb`/Codex `65f19dc2aef826c4`; Zod Claude `77fe5f59f546fd55`/Codex `7386c6352cb9342c`.
15. **Routing margins:** 0.000 for every task (Claude 0.500, Codex 0.500); the minimum remained 0.05.
16. **Pilot outcomes:** FD-001 → `ERROR` after the assigned Claude process completed successfully; FD-002, FD-003, all three HTTPX tasks, and all three Zod tasks → `NOT ATTEMPTED` after the fail-closed stop. Pilot completion was 1/9 task executions and 0/9 independently resolved outcomes.
17. **Integrity:** FD-001 `clean`; the eight unattempted tasks have no integrity result. Candidate commit `79176dcaf3a7a6bf714d21d927b2452806e5b317` changed 3 files, +104/-7, with 2,813 ignored build artifacts excluded by policy.
18. **Evaluators:** FD-001 required `tests` and `lint` were each `INCONCLUSIVE/error` in 56 ms and 64 ms. Both were blocked before command execution because the credential-free evaluator environment did not satisfy the reused sandbox's mandatory `ANTHROPIC_API_KEY` list. No evaluator ran for the remaining tasks.
19. **Forge infrastructure failures:** one release-blocking design/implementation defect produced two recorded `CredentialUnavailable` failures. `Runner::evaluate` correctly constructs `EnvPolicy::conservative`, while `DockerSandbox::wrap` incorrectly requires the agent credential for every process sharing that sandbox.
20. **Agent engineering failures:** zero proven. Claude completed FD-001 with exit 0, but independent evaluation was unavailable, so engineering success/failure is inconclusive. No Codex engineering attempt occurred.
21. **Sandbox violations:** zero observed; no escape, host-path exposure, uncontained child, or cross-run contamination. The run container was removed; only the expected local pilot image registry remained.
22. **Credential violations:** zero exposure or leakage. Two erroneous credential-enforcement events blocked evaluators; this is infrastructure failure, not secret disclosure.
23. **Resource-limit events:** zero OOM, disk-emergency, PID, or workspace-limit events in the live run.
24. **Timeout events:** zero; Claude completed in 353.839 seconds within the 1,800-second limit.
25. **Cleanup:** container cleanup PASS (`SandboxCleaned` recorded); durable run artifacts remained; failed workspace intentionally retained under policy. No later worktree was created.
26. **Artifact retention:** PASS for prompt, provider stream, patch, ledger, routing decisions, events, and failed workspace. Durable run directory was 20 KiB; ledger 808 KiB; retained failed workspace about 702 MiB, of which about 701 MiB was disposable `target/` cache.
27. **Claude accounting:** 1,783,468 input + 17,631 output = 1,801,099 provider-reported tokens; known USD `$1.59772775`; one live Claude run.
28. **Codex accounting:** no live Codex run, tokens, provider credits, or derived credits; coverage is 0/0 attempted Codex tasks and 0/3 assigned tasks.
29. **Backup result:** PASS on the actual one-run fd ledger; schema 12, integrity OK, one run, backup SHA-256 `ef335ea93bac03c5fdcddb8c3192bb95ee7928d9903e934b2ec5f0c5ddfaa5cd`; operational source ledger was not replaced.
30. **Backup duration:** 0.10 seconds wall clock.
31. **Restore result:** PASS into `/private/tmp/forge-pilot-live-recovery.KJ6Vb2/fd-restored`; integrity, foreign keys, schema, and history passed. Source/restored normalized export SHA-256 both `a114946569483fece46849803ea6a3eeb8ed857afa86ade6d8bb076c2974ac0`; router-record SHA-256 both `b169e2738754aeed46a7e1f72a0d2c1b31b6537185c69a3d0dd13e70d5bfc8e5`; counts both 1 run/20 events/3 routing decisions/3 task revisions.
32. **Restore duration:** 0.03 seconds wall clock.
33. **Rollback rehearsal:** PASS WITH LIMITATION. The v1.0.1-tagged source binary (which reports `forge 0.1.0`) read the restored schema-12 `R-0001` when paired with its version-compatible config. It rejected v1.1 `[artifacts]` configuration as expected; rollback must restore both binary and versioned config.
34. **Proposed RPO:** one completed run or 24 hours, whichever is smaller; human approval required.
35. **Proposed RTO:** 15 minutes; human approval required. Measured local restore is far below the target but covers only a single-run local ARM64 ledger.
36. **Retention recommendation:** mandatory ledger/export/patch/integrity/evaluator/routing/fingerprint/backup evidence 365 days; raw provider streams 30 days; failed source workspaces 7 days; successful workspaces remove immediately after verified capture; remove ignored build caches within 24 hours after evidence capture unless cache behavior is under investigation; retain at least max(1 GiB, 5%) free for launch and stop at 512 MiB emergency floor; begin verified off-volume archiving below 10% free or when retained failed workspaces exceed 10 GiB, keeping 30 days local and 365 days archived. Current free space is 8.21%, so the proposed archive threshold is already crossed, but action requires human approval. All values require human approval, and the aborted one-run sample limits confidence.
37. **Failure-drill rerun:** the 14 documented deterministic drills remained green through the 664-test suite, migration/recovery scripts, doctor probes, and Docker fixtures. Qualification nevertheless FAILS because the suite omitted credential-free evaluation inside a credential-bearing required sandbox.
38. **Docker adversarial suite:** PASS, 3/3: host secret/path and network isolation, timeout/descendant cleanup, and typed OOM cleanup.
39. **Migration gate:** PASS; version/migration consistency, schema-7-to-12 staged migration, old-backup migration, and 16 SQLite tests passed.
40. **Dependency/security audit:** PASS using cargo-audit 0.22.2, 1,216 current RustSec advisories, and 247 locked dependencies; no active advisory and no active `rsa` dependency graph.
41. **Security exception:** `RUSTSEC-2023-0071` only, inactive SQLx optional MySQL RSA path; owner Forge release maintainers; review/expiry 2026-11-16.
42. **Version consistency:** PASS; workspace/binary Forge 1.1.0 and migration 12.
43. **Final test count:** 664 passed, 0 failed, 3 ignored in the ordinary workspace run; the 3 ignored Docker tests separately passed. Tier 1 analysis added 8 passing Python tests.
44. **Release gate:** FAIL. Local deterministic gates are green, but live required-containment evaluation is broken, 8/9 tasks are unattempted, the pilot has zero independently resolved outcomes, and remote CI has not run.
45. **Final doctor:** PASS for fd, HTTPX, and Zod. This is insufficient evidence because doctor does not exercise the credential-free evaluator path that failed live.
46. **Package path:** existing pre-pilot artifact `dist/v1.1.0-rc/forge-1.1.0-darwin-arm64.tar.gz`; no fresh RC was staged after the failed pilot.
47. **Package SHA-256:** existing and temporary rebuild both `c1f56b385c9167f831d73ba8144892ac4754dcdef6a99f9f28d215d3a6cd4867`.
48. **Deterministic packaging:** PASS; two regenerations to the same temporary staging path produced byte-identical archives and checksum manifests. No published artifact was changed.
49. **Release-note changes:** `docs/releases/v1.1.0.md` now records the 1/9 fail-closed pilot, router abstentions, cost/tokens, evaluator infrastructure defect, recovery results, and pending remote CI. The pre-pilot dist note was not restaged.
50. **Remote configuration:** none; `git remote -v` is empty.
51. **Remote CI:** PENDING. After remediation, a new reviewed qualification commit, and maintainer approval: `git remote add origin <FORGE_REPOSITORY_URL>`; `git push -u origin main`; `gh run list --branch main`; `gh run watch <RUN_ID>`; `gh run view <RUN_ID> --log-failed`. No remote was added and no push occurred here.
52. **Unresolved production defects:** required-containment evaluator credential coupling; missing regression/doctor coverage for that path; incomplete pilot. The candidate must ensure evaluators remain credential-free while using required containment, then add a regression and repeat qualification on a newly frozen candidate stratum.
53. **Unresolved human operational decisions:** defect remediation review; whether to register a new pilot stratum; network-allowed acceptance; RPO/RTO; retention/archive thresholds; rollback config procedure; remote URL/CI; candidate diff review; and final release approval.
54. **Supervised-production verdict:** `NOT READY FOR SUPERVISED PRODUCTION`.
55. **Autonomous-production verdict:** `AUTONOMOUS PRODUCTION = NOT READY`.
56. **Source files changed:** the pre-existing candidate modifies `crates/forge-cli/src/commands/doctor.rs`, `crates/forge-executor/src/lib.rs`, and `crates/forge-executor/src/sandbox.rs`; qualification assets and reports are under `pilot/v1.1.0-rc/`, `docs/releases/v1.1.0.md`, and `dist/v1.1.0-rc/`. This continuation added no Rust fix.
57. **Tests added:** the candidate already contains two Rust tests (exact harness-version token matching and missing harness runtime) plus the frozen-manifest validator. No regression yet covers the newly exposed evaluator/sandbox defect.
58. **Tier 1 evidence unchanged:** confirmed; the master SHA-256 remained exact after qualification.
59. **Routing threshold:** confirmed unchanged at 0.05.
60. **Router mode:** confirmed recommendation/shadow-only; all nine decisions were made before outcomes and none selected the executing agent.
61. **Automatic actions:** no automatic merge, policy promotion, routing, or team dispatch; human merge remains required.
62. **DeepSeek:** no work performed.
63. **Phase 9:** no work performed.
64. **Commit:** nothing committed.
65. **Tag:** nothing tagged; no `v1.1.0` tag exists.
66. **Push:** nothing pushed.
67. **Publication:** no release or package published.
68. **Final maintainer release commands:** none are justified while the release gate is red. Do not commit/tag/push/publish this candidate as v1.1.0.
69. **Exact blockers:** fix credential-free evaluator execution under required OCI containment without weakening credential isolation; add regression and doctor/qualification coverage; register the changed candidate/config as a new frozen evidence stratum; rerun all 9 supervised tasks from exact immutable baselines; obtain 9 independent evaluator outcomes with zero production incidents; repeat actual-ledger recovery and all local gates; configure an approved remote and obtain green checked-in CI; complete human review of candidate, egress, retention, RPO/RTO, rollback config, release notes, and artifacts.
