BEGIN IMMEDIATE;

CREATE TABLE tool_calls_v31 (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    message_id TEXT NOT NULL REFERENCES messages(id),
    tool_name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'declined', 'cancelled', 'failed')),
    arguments_json TEXT NOT NULL,
    title TEXT,
    metadata_json TEXT NOT NULL,
    output_text TEXT,
    truncated_output_path TEXT,
    error_text TEXT,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER
);

INSERT INTO tool_calls_v31 (
    id,
    session_id,
    message_id,
    tool_name,
    status,
    arguments_json,
    title,
    metadata_json,
    output_text,
    truncated_output_path,
    error_text,
    started_at_ms,
    finished_at_ms
)
SELECT
    id,
    session_id,
    message_id,
    tool_name,
    status,
    arguments_json,
    title,
    metadata_json,
    output_text,
    truncated_output_path,
    error_text,
    started_at_ms,
    finished_at_ms
FROM tool_calls;

DROP TABLE tool_calls;
ALTER TABLE tool_calls_v31 RENAME TO tool_calls;

CREATE INDEX idx_tool_calls_session_started
    ON tool_calls(session_id, started_at_ms ASC);

COMMIT;
