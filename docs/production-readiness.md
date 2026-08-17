# Production readiness

Forge 1.1.0 is a production-hardening candidate, not an autonomous-production
release. The safest current verdict is SUPERVISED PILOT READY after the full
local gate; supervised production and autonomous production remain blocked on
external evidence.

## Operating modes

Development permits containment mode none, manual agents, and verbose local
diagnostics. It is visibly unsafe and doctor returns not ready.

Supervised production requires OCI containment, manual or recommendation-only
selection, human merge approval, verified backups, pinned release artifacts,
and an operator watching failures.

Autonomous production would additionally require prospectively validated
automatic routing, completed external pilots, proven recovery objectives, and
reviewed policy gates. This program does not enable it.

## Readiness matrix

| Capability | Status | Evidence | Remaining blocker | Required before autonomous production? |
|---|---|---|---|---|
| Single-agent supervised execution | READY | Stub integration and pipeline tests | External portability pilot | Yes |
| Independent evaluation/integrity | READY | Typed evaluator and protected-input tests | Pilot across ecosystems | Yes |
| Host containment implementation | READY FOR PILOT | OCI boundary, fail-closed preflight, adversarial fixtures | Validate pinned production images in pilot | Yes |
| Resource/disk controls | READY FOR PILOT | Disk floor/watchdog and typed OOM tests | Pilot drills under real builds | Yes |
| SQLite backup/restore | READY | WAL, corruption, old-schema and rollback tests | Set operational RPO/RTO | Yes |
| Artifact retention/archive | READY FOR PILOT | Typed retention config and archive script | Approve retention schedule/location | Yes |
| Release engineering | READY FOR REVIEW | One version, pinned toolchain, CI, audit, packaging/checksums | Run remote CI and publish reviewed 1.1.0 | Yes |
| Automatic routing | SHADOW ONLY | Exact replay and cutoff/snapshot tests | Prospective holdout not executed | Yes |
| Automatic merge | DISABLED | No implementation | Must remain human until separately authorized | Yes |
| Policy auto-promotion | DISABLED | Explicit promotion only | External evidence and separate authorization | Yes |
| Unattended team auto-dispatch | DISABLED | Explicit validated team path only | Separate safety validation | No for single-agent production |
| External supervised pilot | PENDING | Fully specified pilot and drills | Execute on at least two repositories | Yes |

## Gates

Security requires container escape/secret/network/resource tests and a passing
doctor configuration. Reliability requires disk handling, cleanup, verified
recovery, migration, and interrupted-run drills. Release requires aligned
version, green CI/audit, native artifacts/checksums, notes, and rollback.
Routing requires exact replay plus prospective holdout evidence before automatic
use. Pilot acceptance is defined in external-pilot.md.

Current repository config remains development mode none, so doctor correctly
reports NOT READY for supervised production. Product capability and a specific
deployment's readiness are different claims.

No Phase 9 capability, live agent execution, routing holdout, external pilot,
automatic merge, unattended promotion, automatic team dispatch, commit, or tag
is part of this hardening program.
