# Forge v1.1.0 RC4 human release decision

Recorded: 2026-08-22
Decision authority: human release owner
Intended release mode: supervised production

## Decision

The local Forge `1.1.0` RC4 candidate is accepted for a supervised production
release, subject to the user-controlled GitHub CI/CD and publication gates
listed below.

The frozen RC4 protocol requested nine independently resolved Forge outcomes.
Seven tasks were executed and all seven passed. The release owner explicitly
waives the remaining two executions because the provider API budget was nearly
exhausted and accepts the resulting qualification risk. A waiver is not test
evidence and does not change either task's historical status.

**FROZEN RC4 GATE: not fully satisfied due to 2 human-waived tasks.**

**HUMAN RELEASE DECISION: residual qualification risk accepted.**

**AUTONOMOUS PRODUCTION: NOT AUTHORIZED.**

## Release candidate identity

- Product: Forge `1.1.0`, RC4
- Branch at qualification: `main`
- Historical pre-RC4 HEAD: `be770b858e3aeff90618494b4fc0d133333527ef`
- Product source scope: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, and
  `crates/`
- Product source identity algorithm:
  `sha256-length-framed-relative-path-and-file-bytes-v1`
- Product source identity:
  `8742c698919b3810d26d0795620e11f35eaaf55924d91d008adc7fa9966de839`
- Frozen manifest: `pilot/v1.1.0-rc4/manifest.yaml`
- Frozen manifest SHA-256:
  `314564830961aefad910ec8288875852f52f9bca578e0ac9fb4be1bc3a53776b`
- OCI image:
  `localhost:5000/forge/pilot-runtime@sha256:5624e2d6abe5fb52282963dbd41e1c9e7c1f3a18653bef2726b4c17e42fecde2`
- Image ID:
  `sha256:44deba43a2ed6c8f3d40d551164c7ccc128ab9b04b5f3fdeea525f86fee8b1c3`
- Image platform: Linux ARM64
- Claude Code: `2.1.223`
- Codex CLI: `0.147.0`
- Evaluator substrate: Rust/Cargo `1.93.1`, rustfmt `1.8.0`, Clippy
  `0.1.93`, Node `24.19.0`, pnpm `10.12.1`, Python `3.11.2`, pip
  `23.0.1`, and Git `2.39.5`
- Frozen RC4 Darwin ARM64 qualification archive:
  `dist/v1.1.0-rc4/forge-1.1.0-darwin-arm64.tar.gz`
- Qualification archive SHA-256:
  `13f887a5886456642ddee6e5a73389af19df843f1a783a0457cd5db585aae097`

The product source identity, not the historical starting commit embedded in
the frozen qualification archive metadata, binds the RC4 image and pilot
evidence to the candidate code. A final native release archive is generated
from the local release commit after that commit exists; its metadata and
checksum are recorded in the release handoff rather than back-written into
this decision.

## Exact-image live provider evidence

The release owner observed both production-representative controlled-mutation
probes pass against the exact RC4 image. Successful probe directories are
deleted by design; the durable evidence is this human attestation plus the
frozen image, profile, wrapper, and product identities above.

| Probe | Result | Credential and execution boundary |
| --- | --- | --- |
| Claude controlled mutation | PASS — `production preflight passed` | Only invocation-scoped `ANTHROPIC_API_KEY` reached the wrapper; Claude ran bare inside Forge OCI; evaluator execution remained credential-free; no source change or retained credential |
| Codex controlled mutation | PASS — `production preflight passed` | Only invocation-scoped `CODEX_API_KEY` reached the wrapper; Codex used `inner sandbox=bypassed, boundary=Forge OCI`; no Bubblewrap failure, source change, or retained credential |

The probes establish real authentication, provider execution, controlled
workspace mutation, evaluator credential isolation, and the RC4 Codex
containment remediation. They do not authorize autonomous operation.

## Supervised pilot evidence

The durable ledgers and run artifacts remain in the prepared sibling workspace
`forge-pilot-v1.1.0-rc4`. Every executed run records live provenance, a clean
integrity result, a passing independent evaluation, no infrastructure failure,
a committed candidate patch, and a `WorkspaceDispositionRecorded=removed`
event after durable evidence capture.

| Task | Provider | Ledger run / task revision | Candidate commit | Actual status |
| --- | --- | --- | --- | --- |
| `F-PILOT-FD-001` | Claude | `fd:R-0001` / `TR-2d1e63c8825b91e08b2f8479c3d46bc0c4b8baa283cf28c8e8b2b70ae91d342f` | `46691b3741b7c9da891331dbc39770c54d054a74` | PASS |
| `F-PILOT-FD-002` | Claude | `fd:R-0002` / `TR-3cc8991fed709191dfe463f3920798a0222688d5db87505869d7f4510ddd9fea` | `71f030a8a02f75311034919763d029a30fd67c8f` | PASS |
| `F-PILOT-FD-003` | Claude | `fd:R-0003` / `TR-eaf613b687fe2f904b71edb800d449159f3facdde69e98b06d3dc3655a86047e` | `4a7666620f0cd5732b82ffcb6f4f29cedf1e0478` | PASS |
| `F-PILOT-HTTPX-001` | Codex | `httpx:R-0001` / `TR-230d21c55edf89278f0d34e860d778a3020e04ddef308f4a7f9ff6d83a2580c7` | `5802b2555cdc4a7d17ffa50b837d08bc58798437` | PASS |
| `F-PILOT-HTTPX-002` | Codex | `httpx:R-0002` / `TR-12e3b85f36b8fa0ba8d2422498f50ffafe9c796dec75d474dbdb710ba9b6dc85` | `b84f6115cac8bcf7326811204c6269fbed8d58f7` | PASS |
| `F-PILOT-HTTPX-003` | Codex | `httpx:R-0003` / `TR-1a26be110d6c014c675cc16728701776b9809eaec8daf6d2687325658ac68e35` | `0c7aa3c43e3d3fd4d9e12707532bb57a7d5172b0` | PASS |
| `F-PILOT-ZOD-001` | Claude | `zod:R-0001` / `TR-b55f0198dc2f593492bf118163223d6e6ba37b0694ebc44d86358d113bc2800f` | `4e2bf713f50bb18c1ffb6dbc5f9b9242f07ee645` | PASS |
| `F-PILOT-ZOD-002` | Claude | no run / `TR-a3cd83ed3977ac8f6a53b433aff8ee4c94aaa906b7f6ab02e075f88f14effb1c` | none | **NOT ATTEMPTED — HUMAN WAIVER** |
| `F-PILOT-ZOD-003` | Claude | no run / `TR-eac428fba7bd06ae8f0c98d35ac19e9e2bbe376d740b5e09dff4af6eacff92f0` | none | **NOT ATTEMPTED — HUMAN WAIVER** |

Waiver reason: **Provider API budget constraint; release owner accepts the
residual qualification risk.**

`RC4 pilot: 7/9 executed; 7/7 executed tasks PASS; 2/9 human-waived; 0 observed integrity failures; 0 observed production-class infrastructure failures.`

There were also zero observed required-evaluator execution errors across the
15 evaluator results attached to the seven runs.

## Frozen criteria and validator semantics

The manifest, amendment, assignments, task revisions, and nine-outcome
acceptance criterion remain unchanged. The frozen pre-outcome validator
`pilot/v1.1.0-rc4/validate.py` also intentionally asserts the zero-run state in
which RC4 was frozen, so it is preserved as historical evidence rather than
rewritten after the ledgers advanced.

`pilot/v1.1.0-rc4/validate-release-decision.py` is the separate closure
validator. It verifies the unchanged frozen identities, exact current ledger
outcomes, evaluator results, patch branches, workspace dispositions, two
absent/waived runs, and the required decision language without changing the
original gate.

## Accepted operational limitations

- RC4 uses `network=allowed` because provider calls and dependency installation
  require outbound networking. Forge does not implement destination-level
  egress allowlisting; deployment operators own that control.
- This egress limitation does not grant broad host access. The Docker socket,
  host home, primary worktree, arbitrary host mounts, privileged mode, Linux
  capabilities, and evaluator provider credentials remain unavailable.
- Routing remains recommendation/shadow-only at the frozen `0.05` margin.
  Automatic merge, policy promotion, and team dispatch remain disabled.
- The prospective routing holdout is preregistered but not consumed.
- Native qualification is Darwin ARM64 and the OCI runtime is Linux ARM64;
  publication workflows must produce and test their own platform artifacts.

## External remaining gates

The following actions remain explicitly user-controlled and are not claimed by
this decision:

- configure or verify the GitHub remote;
- push the release commit;
- obtain green GitHub CI and dependency-security results against that exact
  commit;
- push the local annotated `v1.1.0` tag only after the commit checks are green;
- run the tag-triggered release artifact workflow;
- verify published artifact checksums; and
- create the GitHub release or perform any other publication.

If a CI-driven fix changes the product source scope, the RC4 source/image/pilot
binding must be reviewed again. If any fix changes the release commit after a
local tag is created, the unpushed local tag must be recreated at the new
reviewed commit.
