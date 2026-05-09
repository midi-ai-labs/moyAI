ALTER TABLE session_state ADD COLUMN verification_failure_cluster_json TEXT NOT NULL DEFAULT 'null';
ALTER TABLE session_state ADD COLUMN verification_requirement_refs_json TEXT NOT NULL DEFAULT '[]';
