ALTER TABLE session_state
    ADD COLUMN review_scope_json TEXT NOT NULL DEFAULT 'null';

ALTER TABLE session_state
    ADD COLUMN implementation_handoff_json TEXT NOT NULL DEFAULT 'null';
