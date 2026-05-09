CREATE TABLE IF NOT EXISTS session_todos (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    content TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'in_progress', 'completed', 'cancelled')),
    priority TEXT NOT NULL CHECK (priority IN ('high', 'medium', 'low')),
    PRIMARY KEY (session_id, position)
);

CREATE INDEX IF NOT EXISTS idx_session_todos_session_id
    ON session_todos (session_id);
