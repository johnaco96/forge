-- Records the three run statuses separately.
--
-- `status` (added in 0001) is where the run reached in Forge's pipeline.
-- These add the other two: how the agent process ended, and what Forge
-- concluded about the resulting change. They are stored as distinct columns
-- because the interesting queries compare them — "runs where the agent
-- crashed but the patch passed" is a question the ledger should be able to
-- answer directly.

ALTER TABLE runs ADD COLUMN agent_status TEXT;
ALTER TABLE runs ADD COLUMN outcome TEXT;
ALTER TABLE runs ADD COLUMN branch TEXT;

CREATE INDEX runs_outcome_idx ON runs (outcome);
CREATE INDEX runs_agent_status_idx ON runs (agent_status);
