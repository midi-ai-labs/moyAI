CREATE TABLE shared_history_files (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    original_path TEXT NOT NULL,
    local_path TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK(length(sha256)=64),
    PRIMARY KEY(session_id,original_path)
);
CREATE INDEX shared_history_files_local ON shared_history_files(local_path);
INSERT INTO moyai_schema_migrations(version,name) VALUES(68,'shared_history_files');
