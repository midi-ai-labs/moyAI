CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    root_path TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    vcs_kind TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('idle', 'running', 'completed', 'awaiting_user', 'failed')),
    cwd_path TEXT NOT NULL,
    model_name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    parent_message_id TEXT,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    sequence_no INTEGER NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id),
    sequence_no INTEGER NOT NULL,
    part_kind TEXT NOT NULL CHECK (part_kind IN ('text', 'reasoning', 'tool_call', 'tool_result', 'image', 'error', 'diff_summary', 'prompt_dispatch', 'request_diagnostics')),
    payload_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    message_id TEXT NOT NULL REFERENCES messages(id),
    tool_name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    arguments_json TEXT NOT NULL,
    title TEXT,
    metadata_json TEXT NOT NULL,
    output_text TEXT,
    truncated_output_path TEXT,
    error_text TEXT,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER
);

CREATE TABLE IF NOT EXISTS file_changes (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    tool_call_id TEXT NOT NULL REFERENCES tool_calls(id),
    change_kind TEXT NOT NULL CHECK (change_kind IN ('add', 'update', 'delete', 'move')),
    path_before TEXT,
    path_after TEXT,
    before_sha256 TEXT,
    after_sha256 TEXT,
    diff_text TEXT NOT NULL,
    summary_text TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
