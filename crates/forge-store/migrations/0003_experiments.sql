-- Competitive experiments group ordinary runs without copying their evidence.
-- `runs.experiment_id` was reserved in the initial schema and is the normalized
-- one-to-many relation; the experiment keeps only its own configuration,
-- lifecycle, comparison, and raw record.

CREATE TABLE experiments (
    experiment_id   TEXT PRIMARY KEY,
    task_id          TEXT NOT NULL REFERENCES tasks (task_id),
    repository       TEXT NOT NULL,
    base_commit      TEXT NOT NULL,
    agents_json      TEXT NOT NULL,
    status           TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    completed_at     TEXT,
    failure_reason   TEXT,
    comparison_json  TEXT,
    record_json      TEXT NOT NULL
);

CREATE INDEX experiments_task_idx ON experiments (task_id, created_at);
CREATE INDEX experiments_repository_idx ON experiments (repository, created_at);
CREATE INDEX experiments_status_idx ON experiments (status);

-- Experiment lifecycle events are separate from per-run trajectories because
-- their identity and ordering belong to the group, not any one participant.
CREATE TABLE experiment_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    experiment_id  TEXT NOT NULL REFERENCES experiments (experiment_id) ON DELETE CASCADE,
    seq            INTEGER NOT NULL,
    timestamp      TEXT NOT NULL,
    event_type     TEXT NOT NULL,
    data_json      TEXT NOT NULL,
    UNIQUE (experiment_id, seq)
);

CREATE INDEX experiment_events_type_idx ON experiment_events (event_type);
