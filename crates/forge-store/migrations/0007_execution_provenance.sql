-- Routing evidence must distinguish genuine engineering executions from
-- deterministic infrastructure stubs. Older rows cannot be classified from
-- agent names or harness metadata without guessing, so they migrate to the
-- conservative explicit value `unknown`.

ALTER TABLE runs
    ADD COLUMN execution_provenance TEXT NOT NULL DEFAULT 'unknown';

CREATE INDEX runs_routing_provenance_idx
    ON runs (execution_provenance, status, outcome, created_at DESC);
