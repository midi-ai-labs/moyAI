-- Outgoing references retain only public routing metadata and bounded results.
-- Session/turn IDs are immutable audit locators: deletion of local history must
-- not silently erase the only reference with which an external job can be stopped.
CREATE TABLE device_outgoing_references (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=26),
    session_id TEXT NOT NULL CHECK(length(session_id)=26),
    turn_id TEXT NOT NULL CHECK(length(turn_id)=26),
    device_id TEXT NOT NULL CHECK(length(device_id) BETWEEN 1 AND 128),
    profile_id TEXT NOT NULL CHECK(length(profile_id) BETWEEN 1 AND 128),
    root_task_id TEXT NOT NULL CHECK(length(root_task_id) BETWEEN 1 AND 128),
    request_key TEXT NOT NULL CHECK(length(request_key) BETWEEN 1 AND 128),
    prompt_hash TEXT NOT NULL CHECK(length(prompt_hash)=64 AND prompt_hash NOT GLOB '*[^0-9a-f]*'),
    parent_grant_id TEXT,
    parent_job_id TEXT,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json) AND length(CAST(payload_json AS BLOB))<=131072),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms>=0),
    UNIQUE(session_id,turn_id,device_id,profile_id,request_key),
    CHECK(json_extract(payload_json,'$.id') IS id
        AND json_extract(payload_json,'$.session_id') IS session_id
        AND json_extract(payload_json,'$.turn_id') IS turn_id
        AND json_extract(payload_json,'$.device_id') IS device_id
        AND json_extract(payload_json,'$.profile_id') IS profile_id
        AND json_extract(payload_json,'$.root_task_id') IS root_task_id
        AND json_extract(payload_json,'$.request_key') IS request_key
        AND json_extract(payload_json,'$.prompt_hash') IS prompt_hash
        AND json_extract(payload_json,'$.parent_grant_id') IS parent_grant_id
        AND json_extract(payload_json,'$.parent_job_id') IS parent_job_id)
);
CREATE INDEX device_outgoing_references_session_recent ON device_outgoing_references(session_id,CASE WHEN json_extract(payload_json,'$.state') IN ('completed','failed','interrupted') THEN 1 ELSE 0 END,created_at_ms DESC,id DESC);
CREATE INDEX device_outgoing_references_recent ON device_outgoing_references(CASE WHEN json_extract(payload_json,'$.state') IN ('completed','failed','interrupted') THEN 1 ELSE 0 END,created_at_ms DESC,id DESC);
CREATE TRIGGER device_outgoing_references_immutable
BEFORE UPDATE ON device_outgoing_references
WHEN NOT (OLD.id IS NEW.id AND OLD.session_id IS NEW.session_id AND OLD.turn_id IS NEW.turn_id
    AND OLD.device_id IS NEW.device_id AND OLD.profile_id IS NEW.profile_id
    AND OLD.root_task_id IS NEW.root_task_id AND OLD.request_key IS NEW.request_key
    AND OLD.prompt_hash IS NEW.prompt_hash AND OLD.parent_grant_id IS NEW.parent_grant_id
    AND OLD.parent_job_id IS NEW.parent_job_id AND OLD.created_at_ms IS NEW.created_at_ms
    AND (json_extract(OLD.payload_json,'$.job_id') IS NULL
         OR json_extract(OLD.payload_json,'$.job_id') IS json_extract(NEW.payload_json,'$.job_id')))
BEGIN
    SELECT RAISE(ABORT,'outgoing remote job identity is immutable');
END;
INSERT INTO moyai_schema_migrations(version,name) VALUES(63,'device_outgoing_references');
