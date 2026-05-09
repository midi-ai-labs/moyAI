CREATE TABLE IF NOT EXISTS harness_runs (
    id TEXT PRIMARY KEY,
    session_id TEXT REFERENCES sessions(id),
    workspace_root TEXT NOT NULL,
    artifact_root TEXT NOT NULL,
    mode TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    status TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS harness_events (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES harness_runs(id),
    sequence_no INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    contract_refs_json TEXT NOT NULL,
    artifact_refs_json TEXT NOT NULL,
    parent_event_id TEXT,
    payload_sha256 TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(run_id, sequence_no)
);

CREATE TABLE IF NOT EXISTS harness_artifacts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES harness_runs(id),
    kind TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    tags_json TEXT NOT NULL,
    created_by_event_id TEXT,
    contract_refs_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS harness_contracts (
    run_id TEXT NOT NULL REFERENCES harness_runs(id),
    contract_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    version TEXT NOT NULL,
    source_path TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    schema_ref TEXT,
    model_visible_summary TEXT,
    PRIMARY KEY(run_id, contract_id, version)
);

CREATE TABLE IF NOT EXISTS harness_gate_results (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES harness_runs(id),
    sequence_no INTEGER NOT NULL,
    gate_kind TEXT NOT NULL,
    status TEXT NOT NULL,
    severity TEXT NOT NULL,
    owner TEXT,
    summary TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS harness_replay_reports (
    run_id TEXT PRIMARY KEY REFERENCES harness_runs(id),
    schema_version TEXT NOT NULL,
    status TEXT NOT NULL,
    primary_owner TEXT,
    summary TEXT NOT NULL,
    restart_point TEXT,
    next_actions_json TEXT NOT NULL,
    report_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_harness_events_run_sequence
    ON harness_events(run_id, sequence_no ASC);

CREATE INDEX IF NOT EXISTS idx_harness_artifacts_run_kind
    ON harness_artifacts(run_id, kind);

CREATE INDEX IF NOT EXISTS idx_harness_gate_results_run_sequence
    ON harness_gate_results(run_id, sequence_no ASC);

CREATE INDEX IF NOT EXISTS idx_harness_contracts_run_kind
    ON harness_contracts(run_id, kind);

CREATE INDEX IF NOT EXISTS idx_harness_replay_reports_status
    ON harness_replay_reports(status);
