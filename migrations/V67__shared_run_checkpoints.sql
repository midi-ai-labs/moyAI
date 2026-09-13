CREATE TABLE shared_run_checkpoints (
    job_id TEXT PRIMARY KEY CHECK(length(job_id) BETWEEN 1 AND 128),
    project_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id TEXT NOT NULL,
    admission_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active', 'paused')),
    checkpoint_json TEXT CHECK(checkpoint_json IS NULL OR json_valid(checkpoint_json)),
    CHECK(state <> 'paused' OR checkpoint_json IS NOT NULL)
);
CREATE INDEX shared_run_checkpoints_recovery
    ON shared_run_checkpoints(session_id, turn_id, admission_id, state);
-- The Runner mapping owns shared execution permission; ordinary local session settings
-- must not elevate or replace it while the shared job retains this session.
CREATE TRIGGER shared_run_fixed_access_mode
BEFORE UPDATE OF access_mode ON sessions
WHEN NEW.access_mode <> OLD.access_mode
 AND EXISTS(SELECT 1 FROM shared_run_checkpoints WHERE session_id = OLD.id)
BEGIN
    SELECT RAISE(ABORT, 'shared execution access mode is fixed by its Runner environment');
END;
INSERT INTO moyai_schema_migrations(version, name) VALUES(67, 'shared_run_checkpoints');
