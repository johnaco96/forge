-- Runs must retain the task semantics that existed when they were created.
-- `tasks` remains the logical/current identity; immutable revisions hold the
-- exact definition and indexed classification used by historical queries.

CREATE TABLE task_revisions (
    revision_id     TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL REFERENCES tasks (task_id),
    repository      TEXT NOT NULL,
    objective       TEXT NOT NULL,
    category        TEXT,
    language        TEXT,
    domain          TEXT,
    difficulty      TEXT,
    definition_json TEXT NOT NULL,
    created_at      TEXT NOT NULL
);

CREATE INDEX task_revisions_task_idx ON task_revisions (task_id, created_at);
CREATE INDEX task_revisions_repository_idx ON task_revisions (repository, revision_id);
CREATE INDEX task_revisions_category_idx ON task_revisions (category, revision_id);
CREATE INDEX task_revisions_language_idx ON task_revisions (language, revision_id);
CREATE INDEX task_revisions_domain_idx ON task_revisions (domain, revision_id);
CREATE INDEX task_revisions_difficulty_idx ON task_revisions (difficulty, revision_id);

CREATE TABLE task_revision_components (
    revision_id  TEXT NOT NULL REFERENCES task_revisions (revision_id) ON DELETE CASCADE,
    component    TEXT NOT NULL,
    PRIMARY KEY (revision_id, component)
);

CREATE TABLE task_revision_tags (
    revision_id  TEXT NOT NULL REFERENCES task_revisions (revision_id) ON DELETE CASCADE,
    tag          TEXT NOT NULL,
    PRIMARY KEY (revision_id, tag)
);

CREATE INDEX task_revision_components_component_idx
    ON task_revision_components (component, revision_id);
CREATE INDEX task_revision_tags_tag_idx ON task_revision_tags (tag, revision_id);

ALTER TABLE tasks ADD COLUMN current_revision_id TEXT REFERENCES task_revisions (revision_id);
ALTER TABLE runs ADD COLUMN task_revision_id TEXT REFERENCES task_revisions (revision_id);

-- An older database has only one recoverable task definition per logical task.
-- Preserve it as an explicit legacy revision and bind every existing run to it.
INSERT INTO task_revisions (
    revision_id, task_id, repository, objective, category, language, domain,
    difficulty, definition_json, created_at
)
SELECT
    'legacy:' || task_id, task_id, repository, objective, category, language,
    domain, difficulty, definition_json, created_at
FROM tasks;

INSERT INTO task_revision_components (revision_id, component)
SELECT 'legacy:' || task_id, component FROM task_components;

INSERT INTO task_revision_tags (revision_id, tag)
SELECT 'legacy:' || task_id, tag FROM task_tags;

UPDATE tasks SET current_revision_id = 'legacy:' || task_id;
UPDATE runs SET task_revision_id = 'legacy:' || task_id;

CREATE INDEX runs_task_revision_idx ON runs (task_revision_id, created_at DESC);
