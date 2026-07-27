-- Compaction payloads are canonicalized by the Rust migration before this
-- marker is written. Keep this file transaction-free: the caller owns one
-- BEGIN IMMEDIATE boundary around the JSON rewrites and marker.

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (46, 'codex_compaction_checkpoint');
