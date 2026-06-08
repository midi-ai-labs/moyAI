CREATE TABLE IF NOT EXISTS protocol_item_append_order (
    append_position INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    sequence_no INTEGER NOT NULL,
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_kind, source_id)
);

CREATE INDEX IF NOT EXISTS idx_protocol_item_append_order_session_position
    ON protocol_item_append_order(session_id, append_position ASC);

INSERT OR IGNORE INTO protocol_item_append_order (
    session_id,
    turn_id,
    sequence_no,
    source_kind,
    source_id,
    created_at_ms
)
SELECT session_id, turn_id, sequence_no, source_kind, source_id, created_at_ms
FROM (
    SELECT
        session_id,
        turn_id,
        sequence_no,
        'runtime_event' AS source_kind,
        id AS source_id,
        created_at_ms
    FROM protocol_runtime_events
    UNION ALL
    SELECT
        session_id,
        turn_id,
        sequence_no,
        'history_item' AS source_kind,
        id AS source_id,
        created_at_ms
    FROM protocol_history_items
    UNION ALL
    SELECT
        turn_items.session_id,
        turn_items.turn_id,
        turn_items.sequence_no,
        'turn_item' AS source_kind,
        turn_items.id AS source_id,
        COALESCE(history_items.created_at_ms, 0) AS created_at_ms
    FROM protocol_turn_items AS turn_items
    LEFT JOIN protocol_history_items AS history_items
      ON history_items.id = turn_items.source_item_id
)
ORDER BY created_at_ms ASC, sequence_no ASC, source_kind ASC, source_id ASC;
