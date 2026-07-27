ALTER TABLE harness_runs
    ADD COLUMN protocol_turn_id TEXT;

ALTER TABLE harness_runs
    ADD COLUMN canonical_terminal_runtime_event_id TEXT;

CREATE INDEX idx_harness_runs_protocol_turn
    ON harness_runs(session_id, protocol_turn_id)
    WHERE protocol_turn_id IS NOT NULL;

CREATE INDEX idx_harness_runs_canonical_terminal
    ON harness_runs(canonical_terminal_runtime_event_id)
    WHERE canonical_terminal_runtime_event_id IS NOT NULL;

CREATE UNIQUE INDEX idx_harness_events_unique_run_terminal
    ON harness_events(run_id)
    WHERE json_extract(kind, '$') = 'run_terminalized';

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (52, 'harness_turn_identity');
