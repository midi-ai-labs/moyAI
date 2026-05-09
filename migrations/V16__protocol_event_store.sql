CREATE TABLE IF NOT EXISTS protocol_runtime_events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    sequence_no INTEGER NOT NULL,
    msg_json TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(session_id, turn_id, sequence_no)
);

CREATE TABLE IF NOT EXISTS protocol_history_items (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    sequence_no INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(session_id, turn_id, sequence_no)
);

CREATE TABLE IF NOT EXISTS protocol_turn_items (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    source_item_id TEXT,
    sequence_no INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    UNIQUE(session_id, turn_id, sequence_no)
);

CREATE INDEX IF NOT EXISTS idx_protocol_runtime_events_session_turn_sequence
    ON protocol_runtime_events(session_id, turn_id, sequence_no ASC);

CREATE INDEX IF NOT EXISTS idx_protocol_history_items_session_turn_sequence
    ON protocol_history_items(session_id, turn_id, sequence_no ASC);

CREATE INDEX IF NOT EXISTS idx_protocol_turn_items_session_turn_sequence
    ON protocol_turn_items(session_id, turn_id, sequence_no ASC);
