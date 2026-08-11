-- Phase 5 orchestration groups ordinary runs without copying their evidence.
-- Complete JSON preserves the immutable plan and evolving result; normalized
-- tables support history, lineage, and artifact queries.
CREATE TABLE team_executions (
    team_execution_id       TEXT PRIMARY KEY,
    root_task_id            TEXT NOT NULL REFERENCES tasks (task_id),
    root_task_revision_id   TEXT NOT NULL REFERENCES task_revisions (revision_id),
    base_commit             TEXT NOT NULL,
    plan_version            TEXT NOT NULL,
    plan_fingerprint        TEXT NOT NULL,
    plan_source             TEXT NOT NULL,
    execution_provenance    TEXT NOT NULL,
    status                  TEXT NOT NULL,
    outcome                 TEXT,
    final_commit            TEXT,
    baseline_run_id         TEXT REFERENCES runs (run_id),
    created_at              TEXT NOT NULL,
    completed_at            TEXT,
    record_json             TEXT NOT NULL
);

CREATE INDEX team_executions_task_idx
    ON team_executions (root_task_revision_id, created_at DESC);
CREATE INDEX team_executions_status_idx
    ON team_executions (status, outcome, created_at DESC);
CREATE INDEX team_executions_plan_idx
    ON team_executions (plan_fingerprint, created_at DESC);

CREATE TABLE team_nodes (
    team_execution_id       TEXT NOT NULL REFERENCES team_executions (team_execution_id) ON DELETE CASCADE,
    node_id                 TEXT NOT NULL,
    execution_type          TEXT NOT NULL,
    required                INTEGER NOT NULL,
    status                  TEXT NOT NULL,
    node_task_id            TEXT,
    assigned_agent_id       TEXT,
    config_fingerprint      TEXT,
    selection_source        TEXT,
    routing_decision_id     TEXT REFERENCES routing_decisions (decision_id),
    input_commit            TEXT,
    output_commit           TEXT,
    failure_kind            TEXT,
    record_json             TEXT NOT NULL,
    PRIMARY KEY (team_execution_id, node_id)
);

CREATE INDEX team_nodes_status_idx ON team_nodes (status, execution_type);
CREATE INDEX team_nodes_agent_idx ON team_nodes (assigned_agent_id, status);

CREATE TABLE team_edges (
    team_execution_id  TEXT NOT NULL REFERENCES team_executions (team_execution_id) ON DELETE CASCADE,
    from_node_id       TEXT NOT NULL,
    to_node_id         TEXT NOT NULL,
    PRIMARY KEY (team_execution_id, from_node_id, to_node_id),
    FOREIGN KEY (team_execution_id, from_node_id)
        REFERENCES team_nodes (team_execution_id, node_id),
    FOREIGN KEY (team_execution_id, to_node_id)
        REFERENCES team_nodes (team_execution_id, node_id)
);

CREATE TABLE team_artifacts (
    artifact_id         TEXT PRIMARY KEY,
    team_execution_id   TEXT NOT NULL REFERENCES team_executions (team_execution_id) ON DELETE CASCADE,
    producer_node_id    TEXT NOT NULL,
    artifact_kind       TEXT NOT NULL,
    content_sha256      TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    record_json         TEXT NOT NULL,
    FOREIGN KEY (team_execution_id, producer_node_id)
        REFERENCES team_nodes (team_execution_id, node_id)
);

CREATE INDEX team_artifacts_team_idx
    ON team_artifacts (team_execution_id, producer_node_id, artifact_kind);

CREATE TABLE team_node_runs (
    team_execution_id  TEXT NOT NULL,
    node_id            TEXT NOT NULL,
    attempt            INTEGER NOT NULL,
    run_id             TEXT NOT NULL UNIQUE REFERENCES runs (run_id),
    PRIMARY KEY (team_execution_id, node_id, attempt),
    FOREIGN KEY (team_execution_id, node_id)
        REFERENCES team_nodes (team_execution_id, node_id)
);

CREATE INDEX team_node_runs_run_idx ON team_node_runs (run_id);

CREATE TABLE team_events (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    team_execution_id  TEXT NOT NULL REFERENCES team_executions (team_execution_id) ON DELETE CASCADE,
    seq                INTEGER NOT NULL,
    timestamp          TEXT NOT NULL,
    event_type         TEXT NOT NULL,
    data_json          TEXT NOT NULL,
    UNIQUE (team_execution_id, seq)
);

CREATE INDEX team_events_type_idx ON team_events (event_type);
