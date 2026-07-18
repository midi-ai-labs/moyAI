DROP TABLE IF EXISTS session_todos;
DROP TABLE IF EXISTS session_state;

CREATE TABLE IF NOT EXISTS moyai_schema_migrations (
    version INTEGER PRIMARY KEY CHECK (version > 0),
    name TEXT NOT NULL UNIQUE
);

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (32, 'drop_legacy_planner_authority');
