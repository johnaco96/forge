-- Phase 8 self-optimizing engineering policy.
--
-- Immutable policy snapshots plus a mutable pointer to the active one, exactly
-- as Phases 6 and 7 do for world models and health.
--
-- The behavioural record in `record_json` never changes after insertion. Only
-- `status` moves, because a policy's lifecycle is not part of what it does:
-- Draft → Canary → Active → Superseded describes where a policy is, not how it
-- would execute a task. The fingerprint deliberately excludes status, so it
-- stays stable across the whole lifecycle and can be verified on every load.
--
-- Historical Phase 0–7 runs are not touched. The policy columns added to `runs`
-- are nullable and stay NULL for executions that predate Phase 8 — a run that
-- no policy governed must never be made to look as though one did.

CREATE TABLE engineering_policies (
    policy_id         TEXT PRIMARY KEY,
    repository        TEXT NOT NULL,
    parent_policy_id  TEXT REFERENCES engineering_policies (policy_id),
    schema_version    TEXT NOT NULL,
    -- Mutable lifecycle. The behavioural record below is not.
    status            TEXT NOT NULL,
    provenance        TEXT NOT NULL,
    fingerprint       TEXT NOT NULL,
    optimizer_version TEXT,
    -- Deliberately NOT a foreign key. A candidate policy is written before the
    -- proposal that recommends it, because the proposal references the policy;
    -- a constraint here would make the pair unwritable in either order.
    proposal_id       TEXT,
    created_at        TEXT NOT NULL,
    record_json       TEXT NOT NULL
);

CREATE INDEX engineering_policies_repository_idx
    ON engineering_policies (repository, created_at DESC);
CREATE INDEX engineering_policies_status_idx
    ON engineering_policies (repository, status, created_at DESC);
CREATE INDEX engineering_policies_fingerprint_idx
    ON engineering_policies (repository, fingerprint);

-- Mutable pointer to an immutable record. Not UNIQUE on policy_id: a rollback
-- re-points at a policy that was active before, and history keeps both facts.
CREATE TABLE policy_current (
    repository TEXT PRIMARY KEY,
    policy_id  TEXT NOT NULL REFERENCES engineering_policies (policy_id)
);

CREATE TABLE policy_proposals (
    proposal_id          TEXT PRIMARY KEY,
    repository           TEXT NOT NULL,
    active_policy_id     TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    candidate_policy_id  TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    recommendation       TEXT NOT NULL,
    comparison           TEXT NOT NULL,
    evidence_strength    TEXT NOT NULL,
    cutoff               TEXT NOT NULL,
    evidence_fingerprint TEXT NOT NULL,
    optimizer_version    TEXT NOT NULL,
    eligible_count       INTEGER NOT NULL,
    excluded_count       INTEGER NOT NULL,
    created_at           TEXT NOT NULL,
    record_json          TEXT NOT NULL,
    -- The complete evidence snapshot the recommendation was computed from.
    -- Run ids alone would not be enough: health references, the world-model
    -- snapshot, the candidate fingerprints, and each observation's source all
    -- feed `evidence_fingerprint`, so without them a historical proposal could
    -- be read but never re-derived.
    evidence_json        TEXT NOT NULL
);

CREATE INDEX policy_proposals_repository_idx
    ON policy_proposals (repository, created_at DESC);
CREATE INDEX policy_proposals_candidate_idx
    ON policy_proposals (candidate_policy_id, created_at DESC);

-- Queryable projection of the snapshot above, so "which proposals used this
-- run?" is an index lookup rather than a scan over JSON documents.
CREATE TABLE policy_proposal_evidence (
    proposal_id TEXT NOT NULL REFERENCES policy_proposals (proposal_id) ON DELETE CASCADE,
    run_id      TEXT NOT NULL REFERENCES runs (run_id),
    eligible    INTEGER NOT NULL,
    -- NULL when eligible; the typed exclusion discriminant otherwise.
    exclusion   TEXT,
    PRIMARY KEY (proposal_id, run_id)
);

CREATE INDEX policy_proposal_evidence_run_idx
    ON policy_proposal_evidence (run_id);

CREATE TABLE policy_experiments (
    experiment_id           TEXT PRIMARY KEY,
    repository              TEXT NOT NULL,
    control_policy_id       TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    candidate_policy_id     TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    assignment_version      TEXT NOT NULL,
    candidate_share_percent INTEGER NOT NULL,
    status                  TEXT NOT NULL,
    started_at              TEXT NOT NULL,
    concluded_at            TEXT,
    proposal_id             TEXT REFERENCES policy_proposals (proposal_id),
    record_json             TEXT NOT NULL
);

CREATE INDEX policy_experiments_repository_idx
    ON policy_experiments (repository, started_at DESC);
CREATE INDEX policy_experiments_status_idx
    ON policy_experiments (repository, status, started_at DESC);

CREATE TABLE policy_decisions (
    decision_id        TEXT PRIMARY KEY,
    repository         TEXT NOT NULL,
    task_revision_id   TEXT NOT NULL REFERENCES task_revisions (revision_id),
    active_policy_id   TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    selected_policy_id TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    policy_fingerprint TEXT NOT NULL,
    source             TEXT NOT NULL,
    manual_override    TEXT,
    experiment_id      TEXT REFERENCES policy_experiments (experiment_id),
    base_commit        TEXT,
    created_at         TEXT NOT NULL,
    record_json        TEXT NOT NULL
);

CREATE INDEX policy_decisions_repository_idx
    ON policy_decisions (repository, created_at DESC);
CREATE INDEX policy_decisions_policy_idx
    ON policy_decisions (selected_policy_id, created_at DESC);

-- Shadow decisions record a choice and never an outcome. There is deliberately
-- no result column: the shadow-selected strategy did not run, so there is
-- nothing to record about how it fared, and a column here would eventually be
-- filled with a counterfactual nobody observed.
CREATE TABLE policy_shadow_decisions (
    decision_id                TEXT PRIMARY KEY,
    repository                 TEXT NOT NULL,
    task_revision_id           TEXT NOT NULL REFERENCES task_revisions (revision_id),
    active_policy_id           TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    shadow_policy_id           TEXT NOT NULL REFERENCES engineering_policies (policy_id),
    shadow_policy_fingerprint  TEXT NOT NULL,
    actual_selection           TEXT NOT NULL,
    shadow_selection           TEXT NOT NULL,
    agreed                     INTEGER NOT NULL,
    created_at                 TEXT NOT NULL,
    record_json                TEXT NOT NULL
);

CREATE INDEX policy_shadow_decisions_repository_idx
    ON policy_shadow_decisions (repository, created_at DESC);

CREATE TABLE policy_experiment_assignments (
    experiment_id      TEXT NOT NULL REFERENCES policy_experiments (experiment_id) ON DELETE CASCADE,
    task_revision_id   TEXT NOT NULL REFERENCES task_revisions (revision_id),
    arm                TEXT NOT NULL,
    assignment_version TEXT NOT NULL,
    assigned_at        TEXT NOT NULL,
    PRIMARY KEY (experiment_id, task_revision_id)
);

CREATE TABLE policy_experiment_observations (
    experiment_id TEXT NOT NULL REFERENCES policy_experiments (experiment_id) ON DELETE CASCADE,
    run_id        TEXT NOT NULL REFERENCES runs (run_id),
    arm           TEXT NOT NULL,
    recorded_at   TEXT NOT NULL,
    PRIMARY KEY (experiment_id, run_id)
);

CREATE INDEX policy_experiment_observations_run_idx
    ON policy_experiment_observations (run_id);

-- Typed policy subjects, never a run id. Following Phases 5–7: these events are
-- about a policy, a proposal, or an experiment, and attaching them to whichever
-- run happened to be executing would lose that.
CREATE TABLE policy_events (
    subject_kind TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    timestamp    TEXT NOT NULL,
    event_type   TEXT NOT NULL,
    data_json    TEXT NOT NULL,
    PRIMARY KEY (subject_kind, subject_id, seq)
);

CREATE INDEX policy_events_type_idx
    ON policy_events (event_type, timestamp);

-- Nullable linkage on executions. NULL means "no policy governed this run",
-- which is the truth for every Phase 0–7 execution and stays the truth: this
-- migration adds columns and never rewrites a historical row.
ALTER TABLE runs ADD COLUMN policy_id TEXT REFERENCES engineering_policies (policy_id);
ALTER TABLE runs ADD COLUMN policy_fingerprint TEXT;
ALTER TABLE runs ADD COLUMN policy_decision_id TEXT REFERENCES policy_decisions (decision_id);

CREATE INDEX runs_policy_idx ON runs (policy_id, created_at DESC);
CREATE INDEX runs_policy_decision_idx ON runs (policy_decision_id);
