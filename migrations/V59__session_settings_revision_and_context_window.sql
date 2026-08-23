ALTER TABLE sessions
ADD COLUMN session_settings_revision INTEGER NOT NULL DEFAULT 0
    CHECK (
        typeof(session_settings_revision) = 'integer'
        AND session_settings_revision >= 0
    );

INSERT INTO moyai_schema_migrations(version, name)
VALUES (59, 'session_settings_revision_and_context_window');
