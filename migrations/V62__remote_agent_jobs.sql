ALTER TABLE projects ADD COLUMN remote_temp_profile_id TEXT
    CHECK(remote_temp_profile_id IS NULL OR length(remote_temp_profile_id) = 26);
CREATE TRIGGER remote_temp_project_purpose_immutable
BEFORE UPDATE OF remote_temp_profile_id ON projects
WHEN NOT (OLD.remote_temp_profile_id IS NEW.remote_temp_profile_id)
BEGIN
    SELECT RAISE(ABORT, 'remote temp project purpose is immutable');
END;

CREATE TABLE remote_agent_jobs (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id) = 26),
    principal_id TEXT NOT NULL CHECK(length(principal_id) BETWEEN 1 AND 128),
    profile_id TEXT NOT NULL CHECK(length(profile_id) = 26),
    request_key TEXT NOT NULL CHECK(length(request_key) BETWEEN 1 AND 128),
    request_hash TEXT NOT NULL CHECK(length(request_hash) = 64),
    scope_json TEXT NOT NULL CHECK(json_valid(scope_json)),
    parent_json TEXT NOT NULL CHECK(json_valid(parent_json)),
    prompt_preview TEXT NOT NULL CHECK(length(prompt_preview) <= 256),
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE RESTRICT,
    admitted_turn_id TEXT CHECK(admitted_turn_id IS NULL OR length(admitted_turn_id) = 26),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    UNIQUE(principal_id, request_key)
);
CREATE INDEX remote_agent_jobs_profile_created ON remote_agent_jobs(profile_id, created_at_ms DESC, id);
CREATE INDEX remote_agent_jobs_created ON remote_agent_jobs(created_at_ms DESC, id);
CREATE TRIGGER remote_agent_jobs_immutable
BEFORE UPDATE ON remote_agent_jobs
WHEN NOT (
    OLD.id = NEW.id AND OLD.principal_id = NEW.principal_id
    AND OLD.profile_id = NEW.profile_id AND OLD.request_key = NEW.request_key
    AND OLD.request_hash = NEW.request_hash AND OLD.scope_json = NEW.scope_json
    AND OLD.parent_json = NEW.parent_json AND OLD.prompt_preview = NEW.prompt_preview
    AND OLD.session_id = NEW.session_id AND OLD.created_at_ms = NEW.created_at_ms
    AND (NEW.admitted_turn_id IS OLD.admitted_turn_id
        OR (OLD.admitted_turn_id IS NULL AND NEW.admitted_turn_id IS NOT NULL))
)
BEGIN
    SELECT RAISE(ABORT, 'remote job identity is immutable');
END;
INSERT INTO moyai_schema_migrations(version, name) VALUES (62, 'remote_agent_jobs');
