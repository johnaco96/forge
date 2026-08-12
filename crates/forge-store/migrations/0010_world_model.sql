-- Phase 6 immutable, commit-bound repository world models. Full typed JSON is
-- canonical; compact indexes support current/commit/fact/event queries.

CREATE TABLE world_model_snapshots (
    snapshot_id       TEXT PRIMARY KEY,
    repository        TEXT NOT NULL,
    commit_hash       TEXT NOT NULL,
    schema_version    TEXT NOT NULL,
    source             TEXT NOT NULL,
    status             TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    fact_count         INTEGER NOT NULL,
    record_json        TEXT NOT NULL
);

CREATE INDEX world_model_snapshots_commit_idx
    ON world_model_snapshots (repository, commit_hash, created_at DESC);
CREATE INDEX world_model_snapshots_status_idx
    ON world_model_snapshots (repository, status, created_at DESC);

CREATE TABLE world_model_current (
    repository   TEXT PRIMARY KEY,
    snapshot_id  TEXT NOT NULL REFERENCES world_model_snapshots (snapshot_id)
);

CREATE TABLE world_model_extractors (
    snapshot_id       TEXT NOT NULL REFERENCES world_model_snapshots (snapshot_id) ON DELETE CASCADE,
    extractor_name    TEXT NOT NULL,
    extractor_version TEXT NOT NULL,
    required           INTEGER NOT NULL,
    status             TEXT NOT NULL,
    facts_produced     INTEGER NOT NULL,
    record_json        TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, extractor_name)
);

CREATE TABLE world_model_facts (
    snapshot_id     TEXT NOT NULL REFERENCES world_model_snapshots (snapshot_id) ON DELETE CASCADE,
    fact_id         TEXT NOT NULL,
    fact_kind       TEXT NOT NULL,
    display_name    TEXT NOT NULL,
    search_text     TEXT NOT NULL,
    record_json     TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, fact_id)
);

CREATE INDEX world_model_facts_kind_idx
    ON world_model_facts (snapshot_id, fact_kind, display_name);

CREATE TABLE world_model_fact_runs (
    snapshot_id  TEXT NOT NULL,
    fact_id      TEXT NOT NULL,
    run_id       TEXT NOT NULL REFERENCES runs (run_id),
    PRIMARY KEY (snapshot_id, fact_id, run_id),
    FOREIGN KEY (snapshot_id, fact_id)
        REFERENCES world_model_facts (snapshot_id, fact_id) ON DELETE CASCADE
);

CREATE TABLE world_model_fact_tasks (
    snapshot_id  TEXT NOT NULL,
    fact_id      TEXT NOT NULL,
    task_id      TEXT NOT NULL REFERENCES tasks (task_id),
    PRIMARY KEY (snapshot_id, fact_id, task_id),
    FOREIGN KEY (snapshot_id, fact_id)
        REFERENCES world_model_facts (snapshot_id, fact_id) ON DELETE CASCADE
);

CREATE TABLE world_model_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id  TEXT NOT NULL REFERENCES world_model_snapshots (snapshot_id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    timestamp    TEXT NOT NULL,
    event_type   TEXT NOT NULL,
    data_json    TEXT NOT NULL,
    UNIQUE (snapshot_id, seq)
);

CREATE INDEX world_model_events_type_idx ON world_model_events (event_type);

-- Existing systems keep only an optional immutable snapshot reference. Their
-- behavior does not depend on world-model availability.
ALTER TABLE runs ADD COLUMN world_model_snapshot_id TEXT
    REFERENCES world_model_snapshots (snapshot_id);
ALTER TABLE routing_decisions ADD COLUMN world_model_snapshot_id TEXT
    REFERENCES world_model_snapshots (snapshot_id);
ALTER TABLE team_executions ADD COLUMN world_model_snapshot_id TEXT
    REFERENCES world_model_snapshots (snapshot_id);

CREATE INDEX runs_world_model_idx ON runs (world_model_snapshot_id);
CREATE INDEX routing_world_model_idx ON routing_decisions (world_model_snapshot_id);
CREATE INDEX team_world_model_idx ON team_executions (world_model_snapshot_id);
