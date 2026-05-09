CREATE INDEX IF NOT EXISTS idx_sessions_project_updated
    ON sessions(project_id, updated_at_ms DESC);

CREATE INDEX IF NOT EXISTS idx_messages_session_sequence
    ON messages(session_id, sequence_no ASC);

CREATE INDEX IF NOT EXISTS idx_message_parts_message_sequence
    ON message_parts(message_id, sequence_no ASC);

CREATE INDEX IF NOT EXISTS idx_tool_calls_session_started
    ON tool_calls(session_id, started_at_ms ASC);

CREATE INDEX IF NOT EXISTS idx_file_changes_session_created
    ON file_changes(session_id, created_at_ms ASC);
