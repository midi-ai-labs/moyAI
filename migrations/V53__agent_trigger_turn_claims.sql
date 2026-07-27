-- An explicit sub-agent wake must remain attributable after its in-memory worker is gone.
-- Admission owns this immutable link; terminal settlement uses it to distinguish the selected
-- wake from later pending mail and from a replacement turn.

CREATE TABLE agent_trigger_turn_claims (
    history_item_id TEXT PRIMARY KEY
        REFERENCES agent_mailbox_messages(id) ON DELETE CASCADE,
    recipient_session_id TEXT NOT NULL
        REFERENCES sessions(id) ON DELETE CASCADE,
    admission_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    UNIQUE(recipient_session_id, admission_id, turn_id)
);

CREATE INDEX idx_agent_trigger_turn_claims_active_owner
    ON agent_trigger_turn_claims(
        recipient_session_id,
        admission_id,
        turn_id
    );

CREATE TRIGGER validate_agent_trigger_turn_claim_before_insert
BEFORE INSERT ON agent_trigger_turn_claims
WHEN NOT (
    EXISTS (
        SELECT 1
        FROM agent_mailbox_messages AS mailbox
        WHERE mailbox.id = NEW.history_item_id
          AND mailbox.recipient_session_id = NEW.recipient_session_id
          AND mailbox.trigger_turn = 1
          AND (
              mailbox.state = 'pending'
              OR (
                  mailbox.state = 'discarded'
                  AND mailbox.discarded_by_stopped_session_id IS NOT NULL
                  AND mailbox.discarded_after_append_position IS NOT NULL
              )
          )
    )
    AND EXISTS (
        SELECT 1
        FROM sessions AS session
        WHERE session.id = NEW.recipient_session_id
          AND session.status = 'running'
          AND session.active_run_id = NEW.admission_id
          AND session.active_turn_id = NEW.turn_id
          AND session.active_run_lease_expires_at_ms IS NOT NULL
    )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'agent trigger claim must bind one pending explicit wake to its active admitted turn'
    );
END;

CREATE TRIGGER prevent_agent_trigger_turn_claim_update
BEFORE UPDATE ON agent_trigger_turn_claims
BEGIN
    SELECT RAISE(ABORT, 'agent trigger turn claims are immutable');
END;

-- Once admission binds an explicit wake to a turn, every mailbox lifecycle
-- transition must preserve that exact owner.  A normal delivery belongs to the
-- claimed turn, a task-local interruption names that turn's terminal, and an
-- explicit tree stop may discard the pre-boundary wake before its compatible
-- terminal is appended later in the same transaction.
CREATE TRIGGER validate_claimed_agent_mailbox_resolution_before_update
BEFORE UPDATE OF
    state,
    delivered_turn_id,
    delivered_history_item_id,
    resolved_by_terminal_event_id,
    discarded_by_stopped_session_id,
    discarded_after_append_position
ON agent_mailbox_messages
WHEN EXISTS (
    SELECT 1
    FROM agent_trigger_turn_claims AS claim
    WHERE claim.history_item_id = OLD.id
)
AND NOT EXISTS (
    SELECT 1
    FROM agent_trigger_turn_claims AS claim
    WHERE claim.history_item_id = OLD.id
      AND claim.recipient_session_id = OLD.recipient_session_id
      AND (
          (
              NEW.state = 'delivered'
              AND NEW.delivered_turn_id = claim.turn_id
              AND NEW.delivered_history_item_id = NEW.id
              AND NEW.resolved_by_terminal_event_id IS NULL
              AND NEW.discarded_by_stopped_session_id IS NULL
              AND NEW.discarded_after_append_position IS NULL
          )
          OR (
              NEW.state = 'discarded'
              AND NEW.resolved_by_terminal_event_id IS NOT NULL
              AND NEW.discarded_by_stopped_session_id IS NULL
              AND NEW.discarded_after_append_position IS NULL
              AND EXISTS (
                  SELECT 1
                  FROM protocol_runtime_events AS terminal
                  WHERE terminal.id = NEW.resolved_by_terminal_event_id
                    AND terminal.session_id = claim.recipient_session_id
                    AND terminal.turn_id = claim.turn_id
                    AND json_extract(terminal.msg_json, '$.kind')
                        = 'turn_terminal'
                    AND json_extract(
                            terminal.msg_json,
                            '$.terminal.outcome.kind'
                        ) = 'interrupted'
              )
          )
          OR (
              NEW.state = 'discarded'
              AND NEW.resolved_by_terminal_event_id IS NULL
              AND NEW.discarded_by_stopped_session_id IS NOT NULL
              AND NEW.discarded_after_append_position IS NOT NULL
          )
      )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'claimed agent mailbox resolution must preserve its exact turn owner'
    );
END;

INSERT INTO moyai_schema_migrations(version, name)
VALUES (53, 'agent_trigger_turn_claims');
