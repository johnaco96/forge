-- Forge experience ledger, initial schema.
--
-- Shape:
--
--   repository ─ tasks ─ runs ─┬─ events
--                              ├─ patches
--                              └─ evaluations ─ metrics
--
-- Two conventions run through this schema:
--
-- 1. Raw evidence is never discarded. Structured columns exist for querying;
--    a JSON column alongside them keeps the complete record so a later schema
--    can be derived from history rather than from re-running anything.
-- 2. Timestamps are RFC 3339 UTC strings, which sort correctly as text.
--
-- Tables the design anticipates but nothing writes yet (artifacts, commits)
-- are deliberately absent until there is something to put in them.

CREATE TABLE repositories (
    name        TEXT PRIMARY KEY,
    root_path   TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE tasks (
    task_id         TEXT PRIMARY KEY,
    repository      TEXT NOT NULL,
    objective       TEXT NOT NULL,
    task_type       TEXT,
    language        TEXT,
    subsystem       TEXT,
    -- Complete task definition, so a run can be reproduced from the ledger
    -- alone even if the task file changed since.
    definition_json TEXT NOT NULL,
    created_at      TEXT NOT NULL
);

CREATE TABLE agents (
    agent_id          TEXT PRIMARY KEY,
    display_name      TEXT NOT NULL,
    harness           TEXT NOT NULL,
    executable        TEXT,
    adapter_status    TEXT NOT NULL,
    capabilities_json TEXT NOT NULL
);

-- The unit outcomes are compared across: not "Claude Code", but a specific
-- harness/model/tool configuration.
CREATE TABLE agent_configs (
    fingerprint   TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL,
    harness       TEXT NOT NULL,
    model         TEXT,
    tools_json    TEXT NOT NULL,
    settings_json TEXT NOT NULL,
    first_seen_at TEXT NOT NULL
);

CREATE TABLE runs (
    run_id             TEXT PRIMARY KEY,
    task_id            TEXT NOT NULL,
    agent_id           TEXT NOT NULL,
    config_fingerprint TEXT NOT NULL,
    -- Set when a run is part of a competitive experiment.
    experiment_id      TEXT,
    base_commit        TEXT NOT NULL,
    status             TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    started_at         TEXT,
    finished_at        TEXT,
    exit_code          INTEGER,
    failure_reason     TEXT,
    workspace_path     TEXT,
    input_tokens       INTEGER,
    output_tokens      INTEGER,
    cost_usd           REAL,
    record_json        TEXT NOT NULL
);

CREATE INDEX runs_task_idx ON runs (task_id);
CREATE INDEX runs_agent_idx ON runs (agent_id, status);
CREATE INDEX runs_config_idx ON runs (config_fingerprint);
CREATE INDEX runs_experiment_idx ON runs (experiment_id);

-- The trajectory. This is the raw dataset future routing models learn from,
-- so it is append-only and never summarized in place.
CREATE TABLE events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     TEXT NOT NULL REFERENCES runs (run_id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    timestamp  TEXT NOT NULL,
    event_type TEXT NOT NULL,
    data_json  TEXT NOT NULL,
    UNIQUE (run_id, seq)
);

CREATE INDEX events_type_idx ON events (event_type);

CREATE TABLE patches (
    run_id        TEXT PRIMARY KEY REFERENCES runs (run_id) ON DELETE CASCADE,
    base_commit   TEXT NOT NULL,
    head_commit   TEXT,
    files_changed INTEGER NOT NULL,
    insertions    INTEGER NOT NULL,
    deletions     INTEGER NOT NULL,
    diff_path     TEXT
);

CREATE TABLE evaluations (
    run_id          TEXT PRIMARY KEY REFERENCES runs (run_id) ON DELETE CASCADE,
    verdict         TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    finished_at     TEXT NOT NULL,
    checks_json     TEXT NOT NULL,
    dimensions_json TEXT NOT NULL
);

-- Raw measurements, kept in their original units alongside the normalized
-- dimensions, so weightings can change without losing the underlying evidence.
CREATE TABLE metrics (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id    TEXT NOT NULL REFERENCES runs (run_id) ON DELETE CASCADE,
    name      TEXT NOT NULL,
    value     REAL NOT NULL,
    unit      TEXT,
    source    TEXT NOT NULL,
    direction TEXT NOT NULL
);

CREATE INDEX metrics_run_idx ON metrics (run_id, name);
CREATE INDEX metrics_name_idx ON metrics (name);

-- Monotonic id allocation. A counter row survives run deletion, so ids are
-- never reused.
CREATE TABLE counters (
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
