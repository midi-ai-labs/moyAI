ALTER TABLE message_parts RENAME TO message_parts_old;

CREATE TABLE message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id),
    sequence_no INTEGER NOT NULL,
    part_kind TEXT NOT NULL CHECK (
        part_kind IN (
            'text',
            'reasoning',
            'tool_call',
            'tool_result',
            'image',
            'error',
            'diff_summary',
            'prompt_dispatch',
            'request_diagnostics'
        )
    ),
    payload_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

INSERT INTO message_parts (id, message_id, sequence_no, part_kind, payload_json, created_at_ms)
SELECT id, message_id, sequence_no, part_kind, payload_json, created_at_ms
FROM message_parts_old;

DROP TABLE message_parts_old;
