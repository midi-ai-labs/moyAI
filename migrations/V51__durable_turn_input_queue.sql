-- Codex keeps same-turn steering outside conversation history until the next
-- request boundary. The queue owns accepted-but-not-yet-recorded input;
-- canonical protocol history owns input only after delivery.

CREATE TABLE turn_steer_inputs (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    admission_id TEXT,
    turn_id TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    payload_sha256 TEXT NOT NULL,
    origin_kind TEXT NOT NULL
        CHECK(origin_kind IN ('runtime', 'legacy_history', 'fork')),
    state TEXT NOT NULL
        CHECK(state IN ('queued', 'delivered', 'discarded')),
    delivered_history_item_id TEXT UNIQUE
        REFERENCES protocol_history_items(id) ON DELETE CASCADE,
    resolved_by_terminal_event_id TEXT
        REFERENCES protocol_runtime_events(id) ON DELETE RESTRICT,
    accepted_at_ms INTEGER NOT NULL CHECK(accepted_at_ms >= 0),
    delivered_at_ms INTEGER,
    discarded_at_ms INTEGER,
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= accepted_at_ms),
    CHECK(COALESCE(
        json_type(payload_json, '$.kind') = 'text'
        AND json_extract(payload_json, '$.kind') = 'steer_turn',
        0
    )),
    CHECK(COALESCE(
        json_type(payload_json, '$.expected_turn_id') = 'text'
        AND json_extract(payload_json, '$.expected_turn_id') = turn_id,
        0
    )),
    CHECK(
        (
            origin_kind = 'runtime'
            AND admission_id IS NOT NULL
        )
        OR
        (
            origin_kind IN ('legacy_history', 'fork')
            AND admission_id IS NULL
            AND state = 'delivered'
        )
    ),
    CHECK(
        (
            state = 'queued'
            AND origin_kind = 'runtime'
            AND delivered_history_item_id IS NULL
            AND resolved_by_terminal_event_id IS NULL
            AND delivered_at_ms IS NULL
            AND discarded_at_ms IS NULL
        )
        OR
        (
            state = 'delivered'
            AND delivered_history_item_id = id
            AND resolved_by_terminal_event_id IS NULL
            AND delivered_at_ms IS NOT NULL
            AND delivered_at_ms >= accepted_at_ms
            AND discarded_at_ms IS NULL
        )
        OR
        (
            state = 'discarded'
            AND origin_kind = 'runtime'
            AND delivered_history_item_id IS NULL
            AND delivered_at_ms IS NULL
            AND resolved_by_terminal_event_id IS NOT NULL
            AND discarded_at_ms IS NOT NULL
            AND discarded_at_ms >= accepted_at_ms
        )
    )
);

-- The AUTOINCREMENT table is the sole FIFO allocator. Queue rows never infer
-- order from timestamps, UUIDs, MAX()+1, or canonical-history append order.
CREATE TABLE turn_steer_input_enqueue_order (
    enqueue_position INTEGER PRIMARY KEY AUTOINCREMENT,
    input_id TEXT NOT NULL UNIQUE
        REFERENCES turn_steer_inputs(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0)
);

CREATE INDEX idx_turn_steer_inputs_pending
    ON turn_steer_inputs(session_id, turn_id, state);

CREATE INDEX idx_turn_steer_input_enqueue_session_turn
    ON turn_steer_input_enqueue_order(
        session_id,
        turn_id,
        enqueue_position ASC
    );

CREATE INDEX idx_turn_steer_inputs_terminal_resolution
    ON turn_steer_inputs(resolved_by_terminal_event_id)
    WHERE resolved_by_terminal_event_id IS NOT NULL;

-- Every pre-V51 SteerTurn was already canonical history under the old owner.
-- Preserve that fact exactly. There is no evidence from which to reconstruct
-- a pre-V51 pending queue or enqueue order, so migration never invents either.
INSERT INTO turn_steer_inputs (
    id,
    session_id,
    admission_id,
    turn_id,
    payload_json,
    payload_sha256,
    origin_kind,
    state,
    delivered_history_item_id,
    resolved_by_terminal_event_id,
    accepted_at_ms,
    delivered_at_ms,
    discarded_at_ms,
    updated_at_ms
)
SELECT
    history.id,
    history.session_id,
    NULL,
    history.turn_id,
    history.payload_json,
    history.payload_sha256,
    'legacy_history',
    'delivered',
    history.id,
    NULL,
    history.created_at_ms,
    history.created_at_ms,
    NULL,
    history.created_at_ms
FROM protocol_history_items AS history
WHERE history.scope_kind = 'turn'
  AND history.turn_id IS NOT NULL
  AND json_extract(history.payload_json, '$.kind') = 'steer_turn';

CREATE TRIGGER validate_runtime_turn_steer_input_before_insert
BEFORE INSERT ON turn_steer_inputs
WHEN (
        NEW.origin_kind = 'runtime'
        AND (
            NEW.state <> 'queued'
            OR NOT EXISTS (
                SELECT 1
                FROM sessions AS session
                WHERE session.id = NEW.session_id
                  AND session.status = 'running'
                  AND session.active_run_id = NEW.admission_id
                  AND session.active_turn_id = NEW.turn_id
                  AND session.active_run_lease_expires_at_ms IS NOT NULL
                  AND session.active_run_lease_expires_at_ms >= NEW.accepted_at_ms
            )
        )
    )
    OR NEW.origin_kind = 'legacy_history'
    OR (
        NEW.origin_kind = 'fork'
        AND NOT EXISTS (
            SELECT 1
            FROM protocol_history_items AS history
            INNER JOIN protocol_item_append_order AS append_order
              ON append_order.session_id = history.session_id
             AND append_order.scope_kind = history.scope_kind
             AND append_order.turn_id IS history.turn_id
             AND append_order.sequence_no = history.sequence_no
             AND append_order.source_kind = 'history_item'
             AND append_order.source_id = history.id
            WHERE history.id = NEW.id
              AND history.session_id = NEW.session_id
              AND history.scope_kind = 'turn'
              AND history.turn_id = NEW.turn_id
              AND history.payload_sha256 = NEW.payload_sha256
              AND history.payload_json = NEW.payload_json
              AND json_extract(history.payload_json, '$.kind') IS 'steer_turn'
        )
    )
BEGIN
    SELECT RAISE(
        ABORT,
        'turn steer input insert violates active admission or delivered history identity'
    );
END;

CREATE TRIGGER allocate_runtime_turn_steer_input_order_after_insert
AFTER INSERT ON turn_steer_inputs
WHEN NEW.origin_kind = 'runtime'
BEGIN
    INSERT INTO turn_steer_input_enqueue_order (
        input_id,
        session_id,
        turn_id,
        created_at_ms
    )
    VALUES (
        NEW.id,
        NEW.session_id,
        NEW.turn_id,
        NEW.accepted_at_ms
    );
END;

CREATE TRIGGER validate_turn_steer_enqueue_order_before_insert
BEFORE INSERT ON turn_steer_input_enqueue_order
WHEN NOT EXISTS (
    SELECT 1
    FROM turn_steer_inputs AS input
    WHERE input.id = NEW.input_id
      AND input.session_id = NEW.session_id
      AND input.turn_id = NEW.turn_id
      AND input.origin_kind = 'runtime'
      AND input.accepted_at_ms = NEW.created_at_ms
)
BEGIN
    SELECT RAISE(
        ABORT,
        'turn steer enqueue order must reference one runtime input'
    );
END;

CREATE TRIGGER prevent_turn_steer_enqueue_order_update
BEFORE UPDATE ON turn_steer_input_enqueue_order
BEGIN
    SELECT RAISE(ABORT, 'turn steer enqueue order is immutable');
END;

CREATE TRIGGER validate_turn_steer_input_transition_before_update
BEFORE UPDATE ON turn_steer_inputs
WHEN NEW.id IS NOT OLD.id
    OR NEW.session_id IS NOT OLD.session_id
    OR NEW.admission_id IS NOT OLD.admission_id
    OR NEW.turn_id IS NOT OLD.turn_id
    OR NEW.payload_json IS NOT OLD.payload_json
    OR NEW.payload_sha256 IS NOT OLD.payload_sha256
    OR NEW.origin_kind IS NOT OLD.origin_kind
    OR NEW.accepted_at_ms IS NOT OLD.accepted_at_ms
    OR NEW.updated_at_ms < OLD.updated_at_ms
    OR OLD.origin_kind <> 'runtime'
    OR OLD.state <> 'queued'
    OR NEW.state NOT IN ('delivered', 'discarded')
    OR (
        NEW.state = 'delivered'
        AND NOT EXISTS (
            SELECT 1
            FROM protocol_history_items AS history
            INNER JOIN protocol_item_append_order AS append_order
              ON append_order.session_id = history.session_id
             AND append_order.scope_kind = history.scope_kind
             AND append_order.turn_id IS history.turn_id
             AND append_order.sequence_no = history.sequence_no
             AND append_order.source_kind = 'history_item'
             AND append_order.source_id = history.id
            WHERE history.id = NEW.id
              AND history.session_id = NEW.session_id
              AND history.scope_kind = 'turn'
              AND history.turn_id = NEW.turn_id
              AND history.payload_sha256 = NEW.payload_sha256
              AND history.payload_json = NEW.payload_json
              AND json_extract(history.payload_json, '$.kind') IS 'steer_turn'
        )
    )
    OR (
        NEW.state = 'discarded'
        AND NOT EXISTS (
            SELECT 1
            FROM protocol_runtime_events AS terminal
            INNER JOIN protocol_item_append_order AS append_order
              ON append_order.session_id = terminal.session_id
             AND append_order.scope_kind = 'turn'
             AND append_order.turn_id = terminal.turn_id
             AND append_order.sequence_no = terminal.sequence_no
             AND append_order.source_kind = 'runtime_event'
             AND append_order.source_id = terminal.id
            WHERE terminal.id = NEW.resolved_by_terminal_event_id
              AND terminal.session_id = NEW.session_id
              AND terminal.turn_id = NEW.turn_id
              AND json_extract(terminal.msg_json, '$.kind') IS 'turn_terminal'
              AND json_extract(
                    terminal.msg_json,
                    '$.terminal.outcome.kind'
                  ) IS 'interrupted'
              AND terminal.created_at_ms >= NEW.accepted_at_ms
        )
    )
BEGIN
    SELECT RAISE(
        ABORT,
        'turn steer input lifecycle transition violates immutable queue ownership'
    );
END;

INSERT INTO moyai_schema_migrations(version, name)
VALUES (51, 'durable_turn_input_queue');
