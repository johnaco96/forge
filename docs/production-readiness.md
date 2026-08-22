# Production readiness

Forge 1.1.0 RC4 is a **LOCAL RELEASE CANDIDATE READY** for supervised
production under the release owner's explicit two-task qualification waiver.
The release still awaits user-controlled GitHub CI/CD and publication.

**AUTONOMOUS PRODUCTION: NOT AUTHORIZED.**

## Operating modes

Development may use containment mode `none`, manual agents, and verbose local
diagnostics. A Git worktree is repository isolation, not a host security
boundary; doctor reports this mode as not production-ready.

Supervised production is the intended v1.1.0 mode. It requires the pinned RC4
OCI boundary, explicit manual or recommendation-only agent selection, human
review/merge authority, invocation-scoped provider credentials, independent
credential-free evaluation, verified backups, bounded retention, and an
operator monitoring the release window and incident signals.

Autonomous production additionally requires prospective longitudinal evidence
for automatic routing, explicit authorization and rollback criteria for policy
promotion, proven merge safety, unattended dispatch controls, and operational
evidence across more repositories, providers, failures, and time. RC4 does not
enable or authorize any of those actions.

## Readiness matrix

| Capability | Current status | Evidence | Remaining gate or restriction |
| --- | --- | --- | --- |
| Single-agent execution | QUALIFIED FOR SUPERVISED USE | Claude/Codex exact-image probes plus seven live RC4 PASS runs | Human selection and review remain mandatory |
| Independent evaluation/integrity | QUALIFIED | 15/15 live required evaluator results PASS; clean integrity; fail-closed prerequisite and contamination tests | Evaluators remain credential-free and candidate code remains untrusted |
| OCI containment | QUALIFIED | Exact RC4 image, five Docker fixtures, successful provider mutations, no nested Codex sandbox | `network=allowed` egress is operator-managed |
| Credential boundary | QUALIFIED | Per-invocation alternatives, wrapper isolation, redaction, contamination scans, evaluator separation | Rotate and investigate any suspected exposure |
| Resource/disk/process controls | LOCALLY VALIDATED | Preflight floor, watchdog, OOM, timeout, cancellation, process-group, Docker `--init`, and control-call tests | Operator must monitor host/runtime disk and logs |
| Workspace lifecycle | LOCALLY VALIDATED | Central retention matrix, forced contamination destruction, cleanup-failure typing, seven removed-workspace events | Failed-workspace retention is policy-sensitive |
| SQLite/recovery | LOCALLY VALIDATED | WAL-safe backup, sidecar locking, no-clobber publication, staged restore, migration and recovery drills | Take and verify a deployment backup |
| Rollback | LOCALLY VALIDATED | v1.0.1 binary + matching config + verified schema-12 backup rehearsal | Preserve the full three-part deployment unit |
| Evaluator substrate | QUALIFIED | All nine command sets ran in the exact RC4 image | Two expected baseline task signals were not infrastructure failures |
| RC4 external pilot | HUMAN-ACCEPTED WITH WAIVER | 7/9 executed; 7/7 PASS; 2/9 human-waived; no observed integrity/infrastructure failure | Frozen nine-outcome gate is not fully satisfied |
| Release engineering | LOCAL GATES GREEN | Version, format, Clippy, tests, audit, migration, recovery, rollback, deterministic packaging | GitHub CI/CD and publication are external |
| Automatic routing | SHADOW ONLY | Exact replay and preregistered holdout validator | Prospective holdout and explicit authorization |
| Automatic merge/promotion/dispatch | DISABLED | Explicit/manual paths only | Separate evidence and authorization |

## Frozen RC4 boundary

RC4 uses
`localhost:5000/forge/pilot-runtime@sha256:5624e2d6abe5fb52282963dbd41e1c9e7c1f3a18653bef2726b4c17e42fecde2`
on Linux ARM64. Forge's read-only, capability-free, no-new-privileges OCI
container is the trusted process boundary. It mounts only the candidate
worktree writable, the linked-worktree pointer and Git common directory
read-only, and private bounded tmpfs storage. The Docker socket, host home,
primary worktree, arbitrary host paths, privileged mode, and broad Linux
capabilities are unavailable.

Contained Codex uses the documented externally-contained mode instead of
starting Bubblewrap inside that container. Contained Claude runs bare with a
one-shot API-key helper. The wrappers move authentication into the private
ephemeral HOME and remove credential environment variables before
model-directed tools start. Each contained command must explicitly request one
credential allowed by the profile; evaluators request none.

## RC4 qualification decision

Both controlled-mutation live provider probes passed. Seven of nine frozen
tasks were then executed at their exact baseline commits and assignments; all
seven produced live PASS outcomes with clean integrity, no infrastructure
failure, no evaluator execution error, committed patches, and removed
workspaces. `F-PILOT-ZOD-002` and `F-PILOT-ZOD-003` remain **NOT ATTEMPTED —
HUMAN WAIVER** because the provider API budget was nearly exhausted.

The waiver does not satisfy or alter the frozen requirement for nine
independently resolved outcomes.

**FROZEN RC4 GATE: not fully satisfied due to 2 human-waived tasks.**

**HUMAN RELEASE DECISION: residual qualification risk accepted.**

The exact decision and evidence identities are recorded in
`pilot/v1.1.0-rc4/release-decision.md`.

## Network and egress limitation

The RC4 profiles use `network=allowed` because provider calls and reproducible
dependency installation require outbound networking. Forge selects an ordinary
Docker bridge but does not enforce hostname, IP, or destination-level egress
allowlists. The release owner accepts that limitation for supervised use;
deployment operators must supply stricter network policy outside Forge where
required.

Network egress is not unrestricted host access. The mount, capability,
privilege, credential, filesystem, PID, memory, CPU, timeout, and disk controls
above remain in force. Egress denial or provider unavailability is
infrastructure evidence and must never be relabeled as an engineering FAIL.

## Operator responsibilities and failure behavior

Before a production window, the operator must verify the exact binary/config,
run static doctor and one approved live probe per provider, take and verify a
store backup, confirm disk/log/retention capacity, and keep merge authority
human. During the window, stop on credential contamination, integrity failure,
unexpected host access, store corruption, evidence loss, or cleanup
misreporting.

Forge fails closed when required containment, credentials, evaluator tools,
disk capacity, or the container runtime are unavailable. Timeouts and
cancellations terminate process groups and cannot yield PASS. A credential
match blocks durable capture and forces workspace destruction. Other failed
workspaces follow the configured retention policy and record the observed
disposition. Backup/restore and rollback procedures are in `docs/operations.md`.

## Remaining external release gates

- Configure or verify the GitHub remote.
- Push the reviewed release commit on `main`.
- Obtain green CI quality, Docker sandbox, and dependency-security jobs against
  that exact commit.
- Push the `v1.1.0` tag only after those commit checks are green.
- Verify tag-triggered release artifacts and checksums.
- Publish the GitHub release or any packages/images.

No GitHub job or publication is claimed complete. The repository's own
`.forge/config.toml` remains a development configuration and is not a
production deployment attestation; the frozen production profiles are under
`pilot/v1.1.0-rc4`.
