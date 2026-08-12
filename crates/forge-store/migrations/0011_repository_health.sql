-- Phase 7 longitudinal repository health.
--
-- Immutable, commit-bound health snapshots built from world-model facts and
-- persisted engineering evidence. Full typed JSON is canonical, following the
-- Phase 6 pattern; the flat columns exist only to make series, ancestry, and
-- availability queries cheap.
--
-- Diffs and trends are deliberately NOT persisted. They are pure deterministic
-- functions of the snapshots plus a recorded algorithm version, so storing them
-- would create a second copy of a derivable answer that could drift from the
-- evidence. `algorithm_version` travels with every computed result instead.
--
-- Nothing here duplicates run, evaluator, metric, or world-model evidence:
-- measurements reference those records by their existing ids.

CREATE TABLE repository_health_snapshots (
    health_snapshot_id      TEXT PRIMARY KEY,
    repository              TEXT NOT NULL,
    commit_hash             TEXT NOT NULL,
    world_model_snapshot_id TEXT NOT NULL
        REFERENCES world_model_snapshots (snapshot_id),
    schema_version          TEXT NOT NULL,
    builder_version         TEXT NOT NULL,
    status                  TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    dimensions_available    INTEGER NOT NULL,
    measurement_count       INTEGER NOT NULL,
    runs_considered         INTEGER NOT NULL,
    record_json             TEXT NOT NULL
);

CREATE INDEX repository_health_snapshots_commit_idx
    ON repository_health_snapshots (repository, commit_hash, created_at DESC);
CREATE INDEX repository_health_snapshots_status_idx
    ON repository_health_snapshots (repository, status, created_at DESC);

-- Mutable pointer to an immutable snapshot, exactly as Phase 6 does it. A
-- failed build must never replace a successful pointer.
CREATE TABLE repository_health_current (
    repository         TEXT PRIMARY KEY,
    health_snapshot_id TEXT NOT NULL
        REFERENCES repository_health_snapshots (health_snapshot_id)
);

CREATE TABLE repository_health_dimensions (
    health_snapshot_id TEXT NOT NULL
        REFERENCES repository_health_snapshots (health_snapshot_id) ON DELETE CASCADE,
    dimension          TEXT NOT NULL,
    status             TEXT NOT NULL,
    measurement_count  INTEGER NOT NULL,
    PRIMARY KEY (health_snapshot_id, dimension)
);

-- One row per comparable series point. `comparability_key` is the typed
-- identity digest, so a time series is a single indexed lookup and two
-- similarly-named-but-incompatible metrics can never collide.
CREATE TABLE repository_health_measurements (
    health_snapshot_id TEXT NOT NULL
        REFERENCES repository_health_snapshots (health_snapshot_id) ON DELETE CASCADE,
    comparability_key  TEXT NOT NULL,
    dimension          TEXT NOT NULL,
    metric             TEXT NOT NULL,
    unit               TEXT,
    direction          TEXT NOT NULL,
    source             TEXT NOT NULL,
    fingerprint        TEXT,
    component          TEXT,
    value              REAL NOT NULL,
    scope              TEXT NOT NULL,
    observations       INTEGER,
    measured_commit    TEXT NOT NULL,
    PRIMARY KEY (health_snapshot_id, comparability_key)
);

CREATE INDEX repository_health_measurements_series_idx
    ON repository_health_measurements (comparability_key, health_snapshot_id);

-- Pointers into evidence that already exists elsewhere in the ledger.
CREATE TABLE repository_health_evidence (
    health_snapshot_id TEXT NOT NULL
        REFERENCES repository_health_snapshots (health_snapshot_id) ON DELETE CASCADE,
    comparability_key  TEXT NOT NULL,
    evidence_source    TEXT NOT NULL,
    reference          TEXT NOT NULL
);

CREATE INDEX repository_health_evidence_snapshot_idx
    ON repository_health_evidence (health_snapshot_id, comparability_key);

-- Lifecycle events subject to a health snapshot, never to a run.
CREATE TABLE repository_health_events (
    health_snapshot_id TEXT NOT NULL,
    seq                INTEGER NOT NULL,
    timestamp          TEXT NOT NULL,
    event_type         TEXT NOT NULL,
    data_json          TEXT NOT NULL,
    PRIMARY KEY (health_snapshot_id, seq)
);

CREATE INDEX repository_health_events_type_idx
    ON repository_health_events (event_type, timestamp);
