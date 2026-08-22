# Operations runbook

## v1.1.0 supervised operating posture

Forge 1.1.0 is approved locally only for supervised production under the RC4
human release decision. Agent selection is manual or recommendation-only;
merge, routing execution, policy promotion, and team dispatch remain under
explicit human control. **AUTONOMOUS PRODUCTION: NOT AUTHORIZED.**

The release qualification includes successful exact-image Claude and Codex
controlled-mutation probes plus seven passing live pilot runs. Two frozen Zod
tasks were not attempted and are human-waived; this is accepted residual risk,
not PASS evidence. See `pilot/v1.1.0-rc4/release-decision.md` before operating
the release.

## Startup

Run the static preflight before every production window:

    forge doctor

Then run a provider-representative probe for each provider the window will use:

    forge doctor --live-agent-probe --probe-agent claude
    forge doctor --live-agent-probe --probe-agent codex

It checks repository access, package version, active SQLite integrity and
schema, disk floors, containment runtime/image/network, credential presence
without values, configured limits, evaluator prerequisites, and both configured
agent executable locations. The live probe creates a disposable standalone Git
repository and requires exactly one controlled marker mutation; it has a strict
timeout, cleans up on success, and retains redacted diagnostics on failure. It
makes a real provider call and therefore requires an approved job-scoped
credential. Executable presence or a version string alone is never production
proof. A warning or failure is not authorization to bypass required containment.

## Backup and evidence archive

Take a verified SQLite snapshot before upgrades and at the agreed operational
cadence:

    forge store backup --output /backups/forge-YYYYMMDD.db
    forge store verify --path /backups/forge-YYYYMMDD.db

Backup uses SQLite VACUUM INTO, includes committed WAL state, verifies integrity
and foreign keys, and publishes atomically. Never copy forge.db alone while WAL
may be active.

Forge uses a `forge.db.lock` sidecar for cross-process shared/exclusive store
coordination. Preserve it while diagnosing an active deployment, but do not
treat the sidecar itself as the database backup.

Ledger, patches, evaluator logs, tasks, and config are must-retain evidence.
Provider stdout/stderr is optional and controlled by
artifacts.retain_agent_streams. Create a portable archive with:

    FORGE_BIN=target/release/forge scripts/archive-evidence.sh . archive.tar.gz

Add --include-provider-streams only when retention policy permits raw streams.
Build caches, temporary container state, target directories, and removed
worktrees are reproducible/ephemeral and are not archived.

Apply the configured retention schedule to provider streams and failed
workspaces, and move durable archives off the active volume. Reproducible build
caches may be deleted after 24 hours; ledgers, tasks, manifests, patches,
evaluator evidence, and disposition events must not be deleted as cache. Monitor
both percentage and absolute free-space floors. Forge does not rotate host
container-runtime logs; configure that in the operator's Docker logging layer.

## Restore

Restore requires an explicit replacement flag and always verifies/migrates a
separate staging database first:

    forge store restore --from /backups/forge-YYYYMMDD.db --force
    forge store verify

If staging, migration, installation, or final verification fails, the existing
store remains in place or is rolled back. Preserve the original backup.

The non-destructive local drill is:

    scripts/recovery-drill.sh

It creates a temporary repository, backs up an active store, verifies a second
copy, and deletes only its temporary directory.

## Failure response

- Corrupt store: stop new jobs, copy all database sidecars for forensics, run
  store verify, and restore the most recent verified backup. Do not open a
  newer database with an older binary.
- Low disk before a job: free/archive space and rerun doctor. Do not lower both
  absolute and percentage floors to force a start.
- Disk emergency during a job: Forge terminates the process group/container and
  records DiskExhausted. Preserve the run record and failed workspace if quota
  allows; remove reproducible caches first.
- Interrupted run: inspect history and failure events, kill any unexpected
  runtime container, retain or remove the worktree according to
  workspaces.keep_on_failure, then run git worktree prune after human review.
- Sandbox failure: verify Docker service, pinned image digest, restricted
  network, limits, and credential names. Required mode never falls back.
- Provider outage: record it as infrastructure, do not relabel it FAIL, and use
  explicit manual fallback only after the incident is acknowledged.
- Credential rotation: update the job-scoped secret source, never the config
  with a value; doctor checks presence only. Rotate captured provider logs if
  exposure is suspected.
- Migration failure: retain binary, original store, and backup; do not retry
  destructively. Restore and follow the release rollback procedure.

## Cleanup

Successful workspaces are removed unless explicitly retained. Failed
workspaces follow workspaces.keep_on_failure. Candidate branches and durable
evidence are not silently deleted. Container cleanup uses forced runtime
removal, which terminates descendants. Any manual cleanup must resolve an exact
run ID and stay inside the configured worktree root.

Contained Claude accepts only `ANTHROPIC_API_KEY`; contained Codex accepts only
`CODEX_API_KEY`. Their production wrappers move authentication into the private
ephemeral HOME and remove the credential environment before model-directed
tools start. Host login directories and ambient shell credential sets are not
mounted. A candidate credential-value match is an incident: Forge destroys the
workspace, but operators must still rotate the credential and review retained
process/provider evidence.

Production profiles currently require `network=allowed` for provider APIs and
dependency installation. Forge selects the configured Docker network but does
not implement destination allowlisting. Enforce destination policy outside
Forge where required and record any egress denial as infrastructure, never as
an engineering failure.

The release owner accepts this egress limitation for supervised deployment.
It does not expose the Docker socket, host home, primary worktree, arbitrary
host mounts, privileged mode, or broad Linux capabilities. If the deployment
requires destination controls, configure and test them in the operator's
network layer before allowing tasks to run.

## Release rollback

Treat the deployment as a versioned unit: binary, strict configuration, and
database backup. Keep the previous binary together with the exact configuration
that it accepted; a newer TOML file may contain sections an older strict parser
correctly rejects. Stop jobs, preserve the upgraded evidence, and stage the
previous binary plus its version-matched configuration. If that binary supports
the current schema, use a verified snapshot of the current database. Otherwise
restore the verified pre-upgrade database backup with the newer restore tooling
before starting the old binary. Never mix an old binary with a new config or
reverse migrations in place.

The non-destructive deployment-unit rehearsal is:

    scripts/rollback-rehearsal.sh \
      CURRENT_FORGE PREVIOUS_FORGE PREVIOUS_CONFIG VERIFIED_DB_BACKUP SOURCE_REPOSITORY

It clones the source repository into a temporary location, installs the three
version-matched artifacts, proves that the previous binary can read history,
and confirms the input backup was not modified. See releases.md for the full
release gate.
