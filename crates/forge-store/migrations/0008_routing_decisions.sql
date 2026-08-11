-- Phase 4B decisions are durable evidence in their own right. Complete typed
-- JSON preserves explanations while normalized identity/version columns make
-- audits searchable without replaying a router.
CREATE TABLE routing_decisions (
    decision_id                  TEXT PRIMARY KEY,
    run_id                       TEXT REFERENCES runs (run_id),
    task_id                      TEXT NOT NULL REFERENCES tasks (task_id),
    task_revision_id             TEXT NOT NULL REFERENCES task_revisions (revision_id),
    created_at                   TEXT NOT NULL,
    decision_kind                TEXT NOT NULL,
    selected_agent_id            TEXT,
    selected_config_fingerprint  TEXT,
    router_version               TEXT NOT NULL,
    evidence_policy_version      TEXT NOT NULL,
    historical_cutoff            TEXT NOT NULL,
    evidence_fingerprint         TEXT NOT NULL,
    eligible_evidence_count      INTEGER NOT NULL,
    record_json                  TEXT NOT NULL
);

CREATE INDEX routing_decisions_task_idx
    ON routing_decisions (task_revision_id, created_at DESC);
CREATE INDEX routing_decisions_run_idx ON routing_decisions (run_id);
CREATE INDEX routing_decisions_version_idx
    ON routing_decisions (router_version, decision_kind, created_at DESC);

CREATE TABLE routing_decision_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    decision_id  TEXT NOT NULL REFERENCES routing_decisions (decision_id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    timestamp    TEXT NOT NULL,
    event_type   TEXT NOT NULL,
    data_json    TEXT NOT NULL,
    UNIQUE (decision_id, seq)
);

CREATE INDEX routing_decision_events_type_idx
    ON routing_decision_events (event_type);

-- Selection source is independent of execution provenance. Older runs were
-- all explicit/manual because `--agent auto` did not exist before this schema.
ALTER TABLE runs ADD COLUMN selection_source TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE runs ADD COLUMN routing_decision_id TEXT REFERENCES routing_decisions (decision_id);

CREATE INDEX runs_selection_source_idx
    ON runs (selection_source, created_at DESC);
CREATE INDEX runs_routing_decision_idx ON runs (routing_decision_id);
