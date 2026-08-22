# Release engineering

The workspace package version in Cargo.toml is authoritative. Every Forge crate
inherits it, and forge --version must match it. The hardening candidate is
1.1.0. A local annotated release tag may be created only after the reviewed
release commit and all achievable local gates are complete; it is not pushed
or published by the local closure process.

The pinned compiler is recorded in rust-toolchain.toml. Before a release:

    ./scripts/check-version.sh
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    git diff --check
    ./scripts/migration-gate.sh
    ./scripts/recovery-drill.sh
    python3 pilot/v1.1.0-rc4/validate-release-decision.py

CI additionally runs Tier 1 analysis tests, exact Rust replay determinism,
holdout-plan validation, Docker adversarial fixtures, and dependency audit.
Provider credentials and live model calls are forbidden in CI.

A supervised release candidate also requires successful exact-image live probes
for every selected provider, a frozen external-pilot decision, and the
deployment-unit rollback rehearsal:

    forge doctor --live-agent-probe --probe-agent claude
    forge doctor --live-agent-probe --probe-agent codex
    scripts/rollback-rehearsal.sh \
      CURRENT_FORGE PREVIOUS_FORGE PREVIOUS_CONFIG VERIFIED_DB_BACKUP SOURCE_REPOSITORY

These gates use approved job-scoped credentials and evidence outside ordinary
credential-free CI. A static doctor PASS does not replace them.

For RC4, seven of nine frozen tasks were executed and all seven passed. The
release owner human-waived `F-PILOT-ZOD-002` and `F-PILOT-ZOD-003` because the
provider API budget was nearly exhausted. The frozen nine-outcome criterion
therefore remains not fully satisfied; the waiver is explicit risk acceptance,
not PASS evidence. Preserve the frozen pre-outcome validator
`pilot/v1.1.0-rc4/validate.py` unchanged. The separate release-decision
validator checks the observed seven-run/two-waiver state.

## Packaging

Build and package the current native platform:

    cargo build --release --locked -p forge-cli --bin forge
    ./scripts/package-release.sh target/release/forge dist/v1.1.0

The archive contains the binary and machine-readable metadata: version, commit
SHA, platform, architecture, latest migration, and sandbox requirement.
SHA256SUMS covers the archive. Current workflow targets macOS ARM64 and Linux
x86-64 runners; a successful artifact only validates the platform that built
it.

Tag-triggered packaging refuses a tag that differs from v plus the workspace
version. Release notes must include migration changes, routing mode and
threshold, containment/runtime/image requirements, known limitations, and the
readiness matrix. Built binaries are release artifacts, not source files.

The local tag sequence is:

    git tag -a v1.1.0 -m "Forge v1.1.0"
    git rev-parse v1.1.0^{}
    git rev-parse HEAD

The peeled tag must equal the reviewed release commit. Push the commit first,
wait for its GitHub CI and dependency-security jobs, then push the tag. If a CI
fix changes HEAD, delete and recreate only the unpushed local tag after the new
commit completes local review; never move an already published release tag.

## Dependency audit

The security workflow installs the pinned cargo-audit version with Cargo
locked mode and denies warnings. Update the pinned audit tool in a reviewed
change, run it locally, and record any narrowly accepted advisory with an owner
and expiry rather than disabling the job.

The current lockfile contains `rsa` only through SQLx's inactive optional MySQL
driver; Forge enables SQLite only and `cargo tree -i rsa --workspace` is empty.
`RUSTSEC-2023-0071` is therefore narrowly ignored in `.cargo/audit.toml`, owned
by Forge release maintainers, with review/expiry on 2026-11-16. CI separately
fails if `rsa` ever enters the active workspace graph, and the exception must
be removed when SQLx no longer places it in the lockfile.

## Database compatibility and rollback

Fresh schema creation and a representative schema-7 database upgrading through
schema 12 are tested. Migration numbering and the compiled compatibility
constant must agree.

Before installing a new binary, take and verify a backup. To roll back:

1. Stop all Forge jobs.
2. Preserve the upgraded store and artifacts for diagnosis.
3. Reinstall the previous checksummed binary.
4. If it supports the current schema, run store verify and doctor.
5. Otherwise restore the pre-upgrade backup with the newer restore tooling,
   then start the older binary only after verification.

Never reverse SQL migrations in place. Forward-incompatible data requires
backup restoration.
