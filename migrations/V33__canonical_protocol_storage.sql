BEGIN IMMEDIATE;

CREATE TABLE tool_calls_v33 (
    id TEXT PRIMARY KEY,
    history_item_id TEXT NOT NULL UNIQUE
        REFERENCES protocol_history_items(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (
        status IN ('pending', 'running', 'completed', 'declined', 'cancelled', 'failed')
    ),
    truncated_output_path TEXT,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER
);

CREATE TABLE file_changes_v33 (
    id TEXT PRIMARY KEY,
    tool_call_id TEXT NOT NULL REFERENCES tool_calls_v33(id) ON DELETE CASCADE,
    change_kind TEXT NOT NULL CHECK (change_kind IN ('add', 'update', 'delete', 'move')),
    path_before TEXT,
    path_after TEXT,
    before_sha256 TEXT,
    after_sha256 TEXT,
    diff_text TEXT NOT NULL,
    summary_text TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

WITH canonical_tool_items AS (
    SELECT
        id,
        session_id,
        created_at_ms,
        sequence_no,
        CASE
            WHEN json_valid(payload_json)
            THEN json_extract(payload_json, '$.kind')
            ELSE NULL
        END AS item_kind,
        CASE
            WHEN json_valid(payload_json)
            THEN json_extract(payload_json, '$.call_id')
            ELSE NULL
        END AS call_id
    FROM protocol_history_items
),
matched_tool_calls AS (
    SELECT
        legacy.id,
        canonical.id AS history_item_id,
        legacy.status,
        legacy.truncated_output_path,
        legacy.started_at_ms,
        legacy.finished_at_ms,
        ROW_NUMBER() OVER (
            PARTITION BY legacy.id
            ORDER BY canonical.created_at_ms ASC,
                     canonical.sequence_no ASC,
                     canonical.id ASC
        ) AS match_rank
    FROM tool_calls AS legacy
    INNER JOIN canonical_tool_items AS canonical
        ON canonical.session_id = legacy.session_id
       AND canonical.item_kind = 'tool_call'
       AND canonical.call_id = legacy.id
)
INSERT INTO tool_calls_v33 (
    id,
    history_item_id,
    status,
    truncated_output_path,
    started_at_ms,
    finished_at_ms
)
SELECT
    id,
    history_item_id,
    status,
    truncated_output_path,
    started_at_ms,
    finished_at_ms
FROM matched_tool_calls
WHERE match_rank = 1;

INSERT INTO file_changes_v33 (
    id,
    tool_call_id,
    change_kind,
    path_before,
    path_after,
    before_sha256,
    after_sha256,
    diff_text,
    summary_text,
    created_at_ms
)
SELECT
    legacy.id,
    legacy.tool_call_id,
    legacy.change_kind,
    legacy.path_before,
    legacy.path_after,
    legacy.before_sha256,
    legacy.after_sha256,
    legacy.diff_text,
    legacy.summary_text,
    legacy.created_at_ms
FROM file_changes AS legacy
INNER JOIN tool_calls_v33 AS retained_tool
    ON retained_tool.id = legacy.tool_call_id;

DROP TABLE file_changes;
DROP TABLE tool_calls;
DROP TABLE message_parts;
DROP TABLE messages;

ALTER TABLE tool_calls_v33 RENAME TO tool_calls;
ALTER TABLE file_changes_v33 RENAME TO file_changes;

CREATE INDEX idx_tool_calls_started
    ON tool_calls(started_at_ms ASC);

CREATE INDEX idx_file_changes_tool_call_created
    ON file_changes(tool_call_id, created_at_ms ASC);

DROP TABLE IF EXISTS session_todos;
DROP TABLE IF EXISTS session_state;

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (33, 'canonical_protocol_storage');

COMMIT;
