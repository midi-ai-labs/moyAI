CREATE TABLE IF NOT EXISTS protocol_turn_sequence_allocators (
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    next_sequence_no INTEGER NOT NULL CHECK (next_sequence_no >= 0),
    PRIMARY KEY (session_id, turn_id)
);
