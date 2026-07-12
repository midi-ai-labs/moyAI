CREATE TABLE IF NOT EXISTS session_spawn_edges (
    root_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    child_session_id TEXT PRIMARY KEY NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_path TEXT NOT NULL,
    task_name TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(root_session_id, agent_path)
);

CREATE INDEX IF NOT EXISTS idx_session_spawn_edges_root_created
    ON session_spawn_edges(root_session_id, created_at_ms, child_session_id);

CREATE INDEX IF NOT EXISTS idx_session_spawn_edges_parent_created
    ON session_spawn_edges(parent_session_id, created_at_ms, child_session_id);
