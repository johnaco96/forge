# Supervised external pilot

The frozen RC4 pilot used FD (Rust), HTTPX (Python), and Zod (TypeScript) at
three exact baseline commits with nine predeclared tasks. Seven tasks were
executed and all seven passed; `F-PILOT-ZOD-002` and
`F-PILOT-ZOD-003` were **NOT ATTEMPTED — HUMAN WAIVER** because the provider
API budget was nearly exhausted.

`RC4 pilot: 7/9 executed; 7/7 executed tasks PASS; 2/9 human-waived; 0 observed integrity failures; 0 observed production-class infrastructure failures.`

The original protocol requested nine independently resolved outcomes. That
frozen gate remains not fully satisfied. The release owner explicitly accepts
the residual qualification risk for supervised v1.1.0; the two waivers are not
PASS evidence. Exact identities and run records are in
`pilot/v1.1.0-rc4/release-decision.md`.

## RC4 repositories and assignments

| Repository | Baseline | Ecosystem | Frozen provider assignment | Actual executions |
| --- | --- | --- | --- | --- |
| FD | `ee20f426ddf338ac7ead5c5f00ea49258005caaf` | Rust | Claude | 3 PASS |
| HTTPX | `b5addb64f0161ff6bfe94c124ef76f6a1fba5254` | Python | Codex | 3 PASS |
| Zod | `9f0a3d81221e3ab7c09ca4911ef35b54817869a4` | TypeScript | Claude | 1 PASS, 2 human-waived |

The task revisions, profiles, evaluator commands, image digest, and explicit
assignments were frozen before outcomes. Pre-outcome router observations were
shadow-only and all abstained at a zero margin; they had no execution authority.

## Execution policy

- agent selection: preregistered explicit provider;
- router: recommendation/shadow only, persisted before outcome evidence;
- containment: required RC4 OCI image;
- network: `allowed` for provider APIs and reproducible dependency
  installation;
- destination-level egress: operator/deployment-managed, not implemented by
  Forge;
- merge: human diff review and approval;
- policy auto-promotion: disabled;
- team auto-dispatch: disabled;
- automatic merge: disabled; and
- backup: verified before repository sessions and upgrades.

A green evaluation is evidence for human review, never merge authority. The
Docker socket, host home, primary worktree, arbitrary host mounts, privileged
mode, and broad Linux capabilities remain unavailable even though egress is
allowed.

## Required drills

The frozen qualification program used deterministic fixtures, not model-quality
tasks, to demonstrate:

1. interrupted agent and descendant cleanup;
2. provider command failure;
3. timeout;
4. low-disk preflight and emergency termination;
5. memory limit and typed OOM;
6. container unavailable with no host fallback;
7. network-disabled behavior;
8. required credential absent;
9. contained evaluator credential isolation;
10. contained evaluator toolchain completeness;
11. failed-workspace retention when configured;
12. WAL-active backup and staged restore;
13. older-schema upgrade;
14. effective configuration fingerprint drift;
15. stale model or harness version; and
16. failed-workspace cleanup and evidence preservation.

All 16 required drills passed. All five Docker-only tests were also executed
explicitly and passed.

## Frozen acceptance and actual decision

The original acceptance contract requires at least two external repositories
across two ecosystems, every applicable drill, doctor in each production
configuration, no host secret/path escape, equal-history backup restoration,
rehearsed upgrade/rollback, no campaign-blocking Forge defect, and every agreed
live outcome.

RC4 satisfies the repository/ecosystem, drill, exact-image doctor, live-provider,
containment, integrity, recovery, and defect criteria observed in the seven
executed runs. It does not satisfy the nine-outcome criterion because two tasks
were waived. The durable decision therefore states both:

- **FROZEN RC4 GATE: not fully satisfied due to 2 human-waived tasks.**
- **HUMAN RELEASE DECISION: residual qualification risk accepted.**

**AUTONOMOUS PRODUCTION: NOT AUTHORIZED.**
