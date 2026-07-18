BEGIN IMMEDIATE;

ALTER TABLE sessions DROP COLUMN memory_mode;

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (34, 'drop_sessions_memory_mode');

COMMIT;
