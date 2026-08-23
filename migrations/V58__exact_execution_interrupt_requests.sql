CREATE TABLE exact_execution_interrupt_requests (
    session_id TEXT PRIMARY KEY
        REFERENCES sessions(id) ON DELETE CASCADE,
    admission_id TEXT NOT NULL CHECK(length(admission_id) = 26),
    turn_id TEXT NOT NULL CHECK(length(turn_id) = 26),
    admission_revision INTEGER NOT NULL CHECK(
        typeof(admission_revision) = 'integer'
        AND admission_revision > 0
    ),
    cause TEXT NOT NULL CHECK(cause IN ('user_stop', 'agent_interrupted')),
    requested_at_ms INTEGER NOT NULL CHECK(
        typeof(requested_at_ms) = 'integer'
        AND requested_at_ms >= 0
    )
);

CREATE TRIGGER exact_execution_interrupt_requests_validate_before_insert
BEFORE INSERT ON exact_execution_interrupt_requests
WHEN NOT EXISTS (
    SELECT 1
    FROM sessions AS session
    INNER JOIN session_admission_revisions AS revision
      ON revision.session_id = session.id
    WHERE session.id = NEW.session_id
      AND session.status = 'running'
      AND session.active_run_id = NEW.admission_id
      AND session.active_turn_id = NEW.turn_id
      AND session.active_run_lease_expires_at_ms IS NOT NULL
      AND revision.revision = NEW.admission_revision
      AND (
          (
              NEW.cause = 'user_stop'
              AND NOT EXISTS (
                  SELECT 1
                  FROM session_spawn_edges AS edge
                  WHERE edge.child_session_id = NEW.session_id
              )
          )
          OR
          (
              NEW.cause = 'agent_interrupted'
              AND EXISTS (
                  SELECT 1
                  FROM session_spawn_edges AS edge
                  WHERE edge.child_session_id = NEW.session_id
              )
          )
      )
      AND EXISTS (
          SELECT 1
          FROM protocol_turn_sequence_allocators AS allocator
          WHERE allocator.session_id = NEW.session_id
            AND allocator.turn_id = NEW.turn_id
      )
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_runtime_events AS terminal
          WHERE terminal.session_id = NEW.session_id
            AND terminal.turn_id = NEW.turn_id
            AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
      )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'exact execution interrupt request must bind a live exact admission with the matching topology'
    );
END;

CREATE TRIGGER exact_execution_interrupt_requests_prevent_update
BEFORE UPDATE ON exact_execution_interrupt_requests
BEGIN
    SELECT RAISE(ABORT, 'exact execution interrupt requests are immutable');
END;

CREATE TRIGGER exact_execution_interrupt_requests_reject_raw_terminal
BEFORE INSERT ON protocol_runtime_events
WHEN json_extract(NEW.msg_json, '$.kind') = 'turn_terminal'
 AND EXISTS (
     SELECT 1
     FROM exact_execution_interrupt_requests AS request
     WHERE request.session_id = NEW.session_id
       AND request.turn_id = NEW.turn_id
 )
BEGIN
    SELECT RAISE(
        ABORT,
        'pending exact execution interrupt must be consumed before TurnTerminal insertion'
    );
END;

CREATE TRIGGER exact_execution_interrupt_requests_consume_after_session_terminal
AFTER UPDATE OF status ON sessions
WHEN OLD.status = 'running' AND NEW.status <> 'running'
BEGIN
    DELETE FROM exact_execution_interrupt_requests
    WHERE session_id = OLD.id
      AND admission_id = OLD.active_run_id
      AND turn_id = OLD.active_turn_id
      AND admission_revision = (
          SELECT revision
          FROM session_admission_revisions
          WHERE session_id = OLD.id
      );
END;

CREATE TRIGGER exact_execution_interrupt_requests_cleanup_after_turn_delete
AFTER DELETE ON protocol_turn_sequence_allocators
BEGIN
    DELETE FROM exact_execution_interrupt_requests
    WHERE session_id = OLD.session_id
      AND turn_id = OLD.turn_id;
END;

INSERT INTO moyai_schema_migrations(version, name)
VALUES (58, 'exact_execution_interrupt_requests');
