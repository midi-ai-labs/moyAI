BEGIN IMMEDIATE;

CREATE TABLE sessions_v45 (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN ('idle', 'running', 'completed', 'cancelled', 'failed')
    ),
    cwd_path TEXT NOT NULL,
    model_name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    archived_at_ms INTEGER,
    access_mode TEXT NOT NULL DEFAULT 'default'
        CHECK (access_mode IN ('default', 'auto_review', 'full_access')),
    model_parameters_json TEXT NOT NULL DEFAULT '{}',
    active_run_id TEXT,
    active_turn_id TEXT,
    active_run_lease_expires_at_ms INTEGER
);

INSERT INTO sessions_v45 (
    id,
    project_id,
    title,
    status,
    cwd_path,
    model_name,
    base_url,
    created_at_ms,
    updated_at_ms,
    completed_at_ms,
    archived_at_ms,
    access_mode,
    model_parameters_json,
    active_run_id,
    active_turn_id,
    active_run_lease_expires_at_ms
)
SELECT
    id,
    project_id,
    title,
    status,
    cwd_path,
    model_name,
    base_url,
    created_at_ms,
    updated_at_ms,
    completed_at_ms,
    archived_at_ms,
    access_mode,
    model_parameters_json,
    active_run_id,
    active_turn_id,
    active_run_lease_expires_at_ms
FROM sessions;

DROP TABLE sessions;
ALTER TABLE sessions_v45 RENAME TO sessions;

CREATE INDEX idx_sessions_project_updated
    ON sessions(project_id, updated_at_ms DESC);

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (45, 'restore_auto_review_access_mode');

COMMIT;
