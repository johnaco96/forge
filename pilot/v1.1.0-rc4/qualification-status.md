# RC4 qualification status

Status: **LOCAL RELEASE CANDIDATE READY** for supervised production under the
documented human waiver. User-controlled GitHub CI/CD and publication remain
outstanding.

RC4 fixes the known contained-Codex failure without weakening Forge's outer
OCI boundary and closes the process, Git, credential, workspace, evaluator,
and SQLite defects found during the production audit. The practical local test
suite, all five Docker-only tests, dependency audit, migration gate, recovery
drill, rollback rehearsal, deterministic native packaging, pilot/holdout/corpus
validators, Tier 1 analysis tests, and exact-image evaluator command sets are
green or reproduce only their declared baseline task signal.

Both production-representative exact-image provider probes passed. Claude and
Codex each authenticated with only its invocation-scoped credential, executed
inside the Forge OCI boundary, performed the exact disposable marker mutation,
left the source checkout unchanged, and retained no credential. Codex reported
`inner sandbox=bypassed, boundary=Forge OCI`; the RC3 Bubblewrap defect did not
recur. Evaluator execution remained credential-free.

## Actual RC4 pilot outcome

Seven live runs are present across the three prepared RC4 baseline ledgers.
All seven have `completed` pipeline/agent status, live provenance, a committed
patch, clean evaluation integrity, passing required evaluators, no evaluator
execution error, no infrastructure failure, and a durable removed-workspace
disposition.

| RC4 task | Provider | Attempt | Outcome / integrity / infrastructure | Durable evidence |
| --- | --- | --- | --- | --- |
| `F-PILOT-FD-001` | Claude | `fd:R-0001` | PASS / clean / none | patch commit `46691b3741b7c9da891331dbc39770c54d054a74`; workspace removed |
| `F-PILOT-FD-002` | Claude | `fd:R-0002` | PASS / clean / none | patch commit `71f030a8a02f75311034919763d029a30fd67c8f`; workspace removed |
| `F-PILOT-FD-003` | Claude | `fd:R-0003` | PASS / clean / none | patch commit `4a7666620f0cd5732b82ffcb6f4f29cedf1e0478`; workspace removed |
| `F-PILOT-HTTPX-001` | Codex | `httpx:R-0001` | PASS / clean / none | patch commit `5802b2555cdc4a7d17ffa50b837d08bc58798437`; workspace removed |
| `F-PILOT-HTTPX-002` | Codex | `httpx:R-0002` | PASS / clean / none | patch commit `b84f6115cac8bcf7326811204c6269fbed8d58f7`; workspace removed |
| `F-PILOT-HTTPX-003` | Codex | `httpx:R-0003` | PASS / clean / none | patch commit `0c7aa3c43e3d3fd4d9e12707532bb57a7d5172b0`; workspace removed |
| `F-PILOT-ZOD-001` | Claude | `zod:R-0001` | PASS / clean / none | patch commit `4e2bf713f50bb18c1ffb6dbc5f9b9242f07ee645`; workspace removed |
| `F-PILOT-ZOD-002` | Claude | not attempted | **NOT ATTEMPTED — HUMAN WAIVER** | no run or fabricated evidence |
| `F-PILOT-ZOD-003` | Claude | not attempted | **NOT ATTEMPTED — HUMAN WAIVER** | no run or fabricated evidence |

Waiver reason: **Provider API budget constraint; release owner accepts the
residual qualification risk.**

`RC4 pilot: 7/9 executed; 7/7 executed tasks PASS; 2/9 human-waived; 0 observed integrity failures; 0 observed production-class infrastructure failures.`

## Qualification interpretation

The manifest and amendment remain frozen. Their original acceptance criterion
requires nine independently resolved outcomes, and two explicit waivers do not
satisfy or rewrite that criterion.

**FROZEN RC4 GATE: not fully satisfied due to 2 human-waived tasks.**

**HUMAN RELEASE DECISION: residual qualification risk accepted.**

The release owner's durable rationale, exact identities, live-probe
attestation, run/revision/patch identities, and external gates are in
`pilot/v1.1.0-rc4/release-decision.md`. The original
`pilot/v1.1.0-rc4/validate.py` remains an immutable pre-outcome validator and is
not weakened to bless the post-pilot waiver. The separate
`validate-release-decision.py` checks the current seven-run/two-waiver state.

## Remaining external gates and restrictions

No approved Git remote is configured, so GitHub CI/CD has not run and cannot be
claimed green. The user must push the reviewed release commit, verify CI and
dependency-security jobs against that exact SHA, then push the release tag and
publish the resulting artifacts.

Routing remains recommendation/shadow-only at the frozen `0.05` threshold.
Automatic routing, merge, policy promotion, and team dispatch remain disabled.
The RC4 profiles use `network=allowed`; Forge does not implement
destination-level egress allowlisting, so that risk is operator-managed for
supervised deployments.

**AUTONOMOUS PRODUCTION: NOT AUTHORIZED**.
