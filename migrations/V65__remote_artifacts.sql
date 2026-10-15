CREATE TABLE remote_job_inputs (
    job_id TEXT PRIMARY KEY REFERENCES remote_agent_jobs(id) ON DELETE CASCADE,
    payload_json TEXT NOT NULL CHECK(length(CAST(payload_json AS BLOB)) <= 786432 AND json_valid(payload_json))
);
CREATE TABLE remote_job_artifacts (
    job_id TEXT PRIMARY KEY REFERENCES remote_agent_jobs(id) ON DELETE CASCADE,
    version TEXT NOT NULL CHECK(length(version)=64),
    payload_json TEXT NOT NULL CHECK(length(CAST(payload_json AS BLOB)) <= 786432 AND json_valid(payload_json))
);
CREATE TABLE device_artifact_cache (
    reference_id TEXT PRIMARY KEY REFERENCES device_outgoing_references(id) ON DELETE CASCADE,
    job_id TEXT NOT NULL,
    version TEXT NOT NULL CHECK(length(version)=64),
    payload_json TEXT NOT NULL CHECK(length(CAST(payload_json AS BLOB)) <= 786432 AND json_valid(payload_json))
);
CREATE TRIGGER remote_job_inputs_immutable BEFORE UPDATE ON remote_job_inputs
BEGIN SELECT RAISE(ABORT,'remote input version is immutable'); END;
CREATE TRIGGER remote_job_artifacts_immutable BEFORE UPDATE ON remote_job_artifacts
BEGIN SELECT RAISE(ABORT,'remote artifact version is immutable'); END;
CREATE TRIGGER device_artifact_cache_immutable BEFORE UPDATE ON device_artifact_cache
BEGIN SELECT RAISE(ABORT,'received artifact version is immutable'); END;
INSERT INTO moyai_schema_migrations(version,name) VALUES(65,'remote_artifacts');
