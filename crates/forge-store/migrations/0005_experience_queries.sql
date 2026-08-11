-- Phase 3 adds only the task-classification fields needed for indexed
-- historical retrieval. Run evidence remains in the existing normalized
-- tables and complete JSON records.

ALTER TABLE tasks ADD COLUMN category TEXT;
ALTER TABLE tasks ADD COLUMN domain TEXT;
ALTER TABLE tasks ADD COLUMN difficulty TEXT;

-- Existing Phase 0-2 metadata participates immediately.
UPDATE tasks SET category = task_type WHERE category IS NULL;
UPDATE tasks SET domain = subsystem WHERE domain IS NULL;

CREATE INDEX tasks_classification_idx
    ON tasks (repository, category, language, domain, difficulty);
CREATE INDEX tasks_category_idx ON tasks (category, task_id);
CREATE INDEX tasks_language_idx ON tasks (language, task_id);
CREATE INDEX tasks_domain_idx ON tasks (domain, task_id);
CREATE INDEX tasks_difficulty_idx ON tasks (difficulty, task_id);

CREATE TABLE task_components (
    task_id    TEXT NOT NULL REFERENCES tasks (task_id) ON DELETE CASCADE,
    component  TEXT NOT NULL,
    PRIMARY KEY (task_id, component)
);

CREATE TABLE task_tags (
    task_id  TEXT NOT NULL REFERENCES tasks (task_id) ON DELETE CASCADE,
    tag      TEXT NOT NULL,
    PRIMARY KEY (task_id, tag)
);

CREATE INDEX task_components_component_idx ON task_components (component, task_id);
CREATE INDEX task_tags_tag_idx ON task_tags (tag, task_id);

CREATE INDEX runs_history_idx ON runs (created_at DESC, run_id DESC);
CREATE INDEX runs_agent_outcome_history_idx
    ON runs (agent_id, outcome, created_at DESC);
