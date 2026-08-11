-- Phase 2 evaluator results are normalized for direct querying while the
-- complete typed record remains available as JSON for forward compatibility.

ALTER TABLE evaluations ADD COLUMN summary_json TEXT NOT NULL DEFAULT '{}';

CREATE TABLE evaluator_results (
    run_id            TEXT NOT NULL REFERENCES evaluations (run_id) ON DELETE CASCADE,
    evaluator_id      TEXT NOT NULL,
    kind              TEXT NOT NULL,
    required          INTEGER NOT NULL,
    verdict           TEXT NOT NULL,
    execution_status  TEXT NOT NULL,
    duration_ms       INTEGER NOT NULL,
    command           TEXT,
    exit_code         INTEGER,
    artifact_path     TEXT,
    metric_count      INTEGER NOT NULL,
    warning_count     INTEGER NOT NULL,
    execution_error   TEXT,
    result_json       TEXT NOT NULL,
    PRIMARY KEY (run_id, evaluator_id)
);

CREATE INDEX evaluator_results_kind_idx
    ON evaluator_results (kind, verdict, execution_status);
CREATE INDEX evaluator_results_required_idx
    ON evaluator_results (required, verdict);
