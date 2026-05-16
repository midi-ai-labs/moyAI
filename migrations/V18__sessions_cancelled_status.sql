PRAGMA foreign_keys = OFF;

CREATE TABLE IF NOT EXISTS sessions_v18 (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('idle', 'running', 'completed', 'awaiting_user', 'cancelled', 'failed')),
    cwd_path TEXT NOT NULL,
    model_name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER
);

INSERT INTO sessions_v18 (
    id,
    project_id,
    title,
    status,
    cwd_path,
    model_name,
    base_url,
    created_at_ms,
    updated_at_ms,
    completed_at_ms
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
    completed_at_ms
FROM sessions;

DROP TABLE sessions;
ALTER TABLE sessions_v18 RENAME TO sessions;

CREATE INDEX IF NOT EXISTS idx_sessions_project_updated
    ON sessions(project_id, updated_at_ms DESC);

PRAGMA foreign_keys = ON;
