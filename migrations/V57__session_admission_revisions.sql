INSERT INTO protocol_turn_sequence_allocators (session_id, turn_id, next_sequence_no)
SELECT session_id, turn_id, MAX(sequence_no) + 1
FROM (
    SELECT session_id, turn_id, sequence_no FROM protocol_runtime_events
    UNION ALL
    SELECT session_id, turn_id, sequence_no
    FROM protocol_history_items
    WHERE scope_kind = 'turn' AND turn_id IS NOT NULL
    UNION ALL
    SELECT session_id, turn_id, sequence_no FROM protocol_turn_items
)
GROUP BY session_id, turn_id
ON CONFLICT(session_id, turn_id) DO NOTHING;

CREATE TABLE session_admission_revisions (
    session_id TEXT PRIMARY KEY
        REFERENCES sessions(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL DEFAULT 0
        CHECK (typeof(revision) = 'integer' AND revision >= 0)
);

INSERT INTO session_admission_revisions (session_id, revision)
SELECT session.id, COUNT(allocator.turn_id)
FROM sessions AS session
LEFT JOIN protocol_turn_sequence_allocators AS allocator
  ON allocator.session_id = session.id
GROUP BY session.id;

CREATE TRIGGER session_admission_revisions_after_session_insert
AFTER INSERT ON sessions
BEGIN
    INSERT INTO session_admission_revisions (session_id, revision)
    VALUES (NEW.id, 0);
END;

CREATE TRIGGER session_admission_revisions_after_turn_insert
AFTER INSERT ON protocol_turn_sequence_allocators
BEGIN
    UPDATE session_admission_revisions
    SET revision = revision + 1
    WHERE session_id = NEW.session_id
      AND revision < 9223372036854775807;

    SELECT CASE WHEN changes() <> 1 THEN
        RAISE(ABORT, 'session admission revision is missing or exhausted')
    END;
END;

CREATE TRIGGER session_admission_revisions_after_turn_delete
AFTER DELETE ON protocol_turn_sequence_allocators
WHEN EXISTS (SELECT 1 FROM sessions WHERE id = OLD.session_id)
BEGIN
    UPDATE session_admission_revisions
    SET revision = revision + 1
    WHERE session_id = OLD.session_id
      AND revision < 9223372036854775807;

    SELECT CASE WHEN changes() <> 1 THEN
        RAISE(ABORT, 'session admission revision is missing or exhausted')
    END;
END;

INSERT INTO moyai_schema_migrations (version, name)
VALUES (57, 'session_admission_revisions');
