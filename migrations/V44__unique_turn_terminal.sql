CREATE UNIQUE INDEX idx_protocol_runtime_events_unique_turn_terminal
    ON protocol_runtime_events(session_id, turn_id)
    WHERE json_extract(msg_json, '$.kind') = 'turn_terminal';

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (44, 'unique_turn_terminal');
