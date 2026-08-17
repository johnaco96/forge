# Operations runbook

## Startup

Run the read-only preflight before every production window:

    forge doctor

It checks repository access, package version, active SQLite integrity and
schema, disk floors, containment runtime/image/network, credential presence
without values, configured limits, and the default agent executable location.
A warning or failure is not authorization to bypass required containment.

## Backup and evidence archive

Take a verified SQLite snapshot before upgrades and at the agreed operational
cadence:

    forge store backup --output /backups/forge-YYYYMMDD.db
    forge store verify --path /backups/forge-YYYYMMDD.db

Backup uses SQLite VACUUM INTO, includes committed WAL state, verifies integrity
and foreign keys, and publishes atomically. Never copy forge.db alone while WAL
may be active.

Ledger, patches, evaluator logs, tasks, and config are must-retain evidence.
Provider stdout/stderr is optional and controlled by
artifacts.retain_agent_streams. Create a portable archive with:

    FORGE_BIN=target/release/forge scripts/archive-evidence.sh . archive.tar.gz

Add --include-provider-streams only when retention policy permits raw streams.
Build caches, temporary container state, target directories, and removed
worktrees are reproducible/ephemeral and are not archived.

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

## Release rollback

Stop jobs, back up the current store, restore the prior binary, and verify
database compatibility. If the prior binary cannot read the forward schema,
restore the backup taken before the upgrade. See releases.md for the full gate.

