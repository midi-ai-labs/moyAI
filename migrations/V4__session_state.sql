CREATE TABLE IF NOT EXISTS session_state (
    session_id TEXT NOT NULL PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    task_route TEXT NOT NULL DEFAULT 'code' CHECK (task_route IN ('code', 'docs', 'review', 'debug', 'ask', 'summary')),
    phase TEXT NOT NULL CHECK (phase IN ('discovery', 'planning', 'editing', 'verifying', 'repairing', 'completing')),
    review_scope_json TEXT NOT NULL DEFAULT 'null',
    active_todo_id TEXT NULL,
    active_targets_json TEXT NOT NULL,
    failure_kind TEXT NULL CHECK (failure_kind IN ('invalid_tool', 'tool_execution', 'patch_mismatch', 'verification_failed', 'context_overflow', 'provider_retryable', 'provider_fatal', 'completion_drift')),
    failure_summary TEXT NULL,
    failure_tool_name TEXT NULL,
    failure_targets_json TEXT NOT NULL,
    verification_todo_id TEXT NULL,
    verification_commands_json TEXT NOT NULL,
    verification_failures_json TEXT NOT NULL,
    verification_evidence_summary TEXT NULL,
    completion_closeout_ready INTEGER NOT NULL,
    completion_open_work_count INTEGER NOT NULL,
    completion_verification_pending INTEGER NOT NULL,
    completion_blocked_reason TEXT NULL,
    implementation_handoff_json TEXT NOT NULL DEFAULT 'null',
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_state_session_id
    ON session_state (session_id);
