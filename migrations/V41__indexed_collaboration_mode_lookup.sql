CREATE INDEX idx_protocol_history_collaboration_mode_session
    ON protocol_history_items(session_id, id)
    WHERE json_valid(payload_json)
      AND json_extract(payload_json, '$.kind') = 'collaboration_mode_instruction';

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (41, 'indexed_collaboration_mode_lookup');
