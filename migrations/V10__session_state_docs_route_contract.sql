ALTER TABLE session_state
    ADD COLUMN completion_route_contract_pending INTEGER NOT NULL DEFAULT 0;

ALTER TABLE session_state
    ADD COLUMN completion_route_contract_summary TEXT NULL;

ALTER TABLE session_state
    ADD COLUMN docs_route_state_json TEXT NOT NULL DEFAULT 'null';
