CREATE TABLE session_todos_v5 (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    todo_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    content TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('work', 'verification', 'repair', 'completion')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'in_progress', 'blocked', 'completed', 'cancelled')),
    priority TEXT NOT NULL CHECK (priority IN ('high', 'medium', 'low')),
    targets_json TEXT NOT NULL,
    depends_on_json TEXT NOT NULL,
    success_criteria_json TEXT NOT NULL,
    blocked_by_json TEXT NOT NULL,
    PRIMARY KEY (session_id, todo_id),
    UNIQUE (session_id, position)
);

INSERT INTO session_todos_v5 (
    session_id,
    todo_id,
    position,
    content,
    kind,
    status,
    priority,
    targets_json,
    depends_on_json,
    success_criteria_json,
    blocked_by_json
)
SELECT
    session_id,
    upper(substr(hex(randomblob(16)), 1, 26)),
    position,
    content,
    'work',
    status,
    priority,
    '[]',
    '[]',
    '[]',
    '[]'
FROM session_todos;

DROP TABLE session_todos;
ALTER TABLE session_todos_v5 RENAME TO session_todos;

CREATE INDEX idx_session_todos_session_id
    ON session_todos (session_id);

CREATE INDEX idx_session_todos_session_status_position
    ON session_todos (session_id, status, position);
