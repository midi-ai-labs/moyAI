CREATE TABLE agent_owner_resume_requests (
    owner_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    source_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    source_history_item_id TEXT NOT NULL
        REFERENCES protocol_history_items(id) ON DELETE CASCADE,
    state TEXT NOT NULL
        CHECK(state IN ('pending', 'claimed', 'resolved', 'cancelled')),
    claimed_turn_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    claimed_at_ms INTEGER,
    resolved_at_ms INTEGER,
    PRIMARY KEY(owner_session_id, source_history_item_id),
    CHECK(owner_session_id = source_session_id),
    CHECK(
        (state = 'pending'
         AND claimed_turn_id IS NULL
         AND claimed_at_ms IS NULL
         AND resolved_at_ms IS NULL)
        OR
        (state = 'claimed'
         AND claimed_turn_id IS NOT NULL
         AND claimed_at_ms IS NOT NULL
         AND resolved_at_ms IS NULL)
        OR
        (state = 'resolved'
         AND claimed_turn_id IS NOT NULL
         AND claimed_at_ms IS NOT NULL
         AND resolved_at_ms IS NOT NULL)
        OR
        (state = 'cancelled'
         AND resolved_at_ms IS NOT NULL
         AND (
             (claimed_turn_id IS NULL AND claimed_at_ms IS NULL)
             OR
             (claimed_turn_id IS NOT NULL AND claimed_at_ms IS NOT NULL)
         ))
    ),
    CHECK(updated_at_ms >= created_at_ms),
    CHECK(claimed_at_ms IS NULL OR claimed_at_ms >= created_at_ms),
    CHECK(resolved_at_ms IS NULL OR resolved_at_ms >= claimed_at_ms)
);

CREATE INDEX idx_agent_owner_resume_pending
    ON agent_owner_resume_requests(
        owner_session_id,
        created_at_ms,
        source_history_item_id
    )
    WHERE state = 'pending';

CREATE INDEX idx_agent_owner_resume_claimed_turn
    ON agent_owner_resume_requests(owner_session_id, claimed_turn_id)
    WHERE state = 'claimed';

CREATE TRIGGER validate_agent_owner_resume_request_before_insert
BEFORE INSERT ON agent_owner_resume_requests
WHEN NOT (
    NEW.state = 'pending'
    AND NEW.owner_session_id = NEW.source_session_id
    AND EXISTS (
        SELECT 1
        FROM agent_completion_handoffs AS handoff
        INNER JOIN session_spawn_edges AS child_edge
          ON child_edge.child_session_id = handoff.child_session_id
         AND child_edge.parent_session_id = NEW.owner_session_id
        INNER JOIN session_spawn_edges AS owner_edge
          ON owner_edge.root_session_id = child_edge.root_session_id
         AND owner_edge.child_session_id = NEW.owner_session_id
        INNER JOIN protocol_history_items AS source_history
          ON source_history.id = handoff.parent_history_item_id
         AND source_history.id = NEW.source_history_item_id
         AND source_history.session_id = NEW.owner_session_id
         AND source_history.scope_kind = 'session'
         AND json_extract(source_history.payload_json, '$.kind')
             = 'inter_agent_communication'
         AND json_extract(
               source_history.payload_json,
               '$.communication.trigger_turn'
             ) = 0
        WHERE handoff.parent_session_id = NEW.owner_session_id
    )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'owner resume request must link a non-root owner to a direct-child session-scoped FINAL handoff'
    );
END;

CREATE TRIGGER validate_agent_owner_resume_request_before_update
BEFORE UPDATE ON agent_owner_resume_requests
WHEN
    NEW.owner_session_id <> OLD.owner_session_id
    OR NEW.source_session_id <> OLD.source_session_id
    OR NEW.source_history_item_id <> OLD.source_history_item_id
    OR NEW.created_at_ms <> OLD.created_at_ms
    OR (
        OLD.state = 'pending'
        AND NEW.state NOT IN ('pending', 'claimed', 'cancelled')
    )
    OR (
        OLD.state = 'claimed'
        AND NEW.state NOT IN ('pending', 'claimed', 'resolved', 'cancelled')
    )
    OR (
        OLD.state = 'resolved'
        AND (
            NEW.state <> 'resolved'
            OR NEW.claimed_turn_id <> OLD.claimed_turn_id
            OR NEW.claimed_at_ms <> OLD.claimed_at_ms
            OR NEW.resolved_at_ms <> OLD.resolved_at_ms
            OR NEW.updated_at_ms <> OLD.updated_at_ms
        )
    )
    OR (
        OLD.state = 'cancelled'
        AND (
            NEW.state <> 'cancelled'
            OR NEW.claimed_turn_id IS NOT OLD.claimed_turn_id
            OR NEW.claimed_at_ms IS NOT OLD.claimed_at_ms
            OR NEW.resolved_at_ms IS NOT OLD.resolved_at_ms
            OR NEW.updated_at_ms <> OLD.updated_at_ms
        )
    )
    OR (
        OLD.state = 'claimed'
        AND NEW.state IN ('claimed', 'resolved')
        AND (
            NEW.claimed_turn_id <> OLD.claimed_turn_id
            OR NEW.claimed_at_ms <> OLD.claimed_at_ms
        )
    )
    OR (
        OLD.state = 'pending'
        AND NEW.state = 'cancelled'
        AND (
            NEW.claimed_turn_id IS NOT NULL
            OR NEW.claimed_at_ms IS NOT NULL
        )
    )
    OR (
        OLD.state = 'claimed'
        AND NEW.state = 'cancelled'
        AND (
            NEW.claimed_turn_id IS NOT OLD.claimed_turn_id
            OR NEW.claimed_at_ms IS NOT OLD.claimed_at_ms
        )
    )
    OR (
        NEW.state IN ('claimed', 'cancelled')
        AND NEW.claimed_turn_id IS NOT NULL
        AND NOT EXISTS (
            SELECT 1
            FROM protocol_runtime_events AS started
            WHERE started.session_id = NEW.owner_session_id
              AND started.turn_id = NEW.claimed_turn_id
              AND json_extract(started.msg_json, '$.kind') = 'warning'
              AND json_extract(started.msg_json, '$.message')
                  LIKE 'thread started:%'
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'invalid owner resume request state transition');
END;

CREATE TABLE agent_deferred_completions (
    agent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    agent_turn_id TEXT NOT NULL,
    terminal_event_id TEXT NOT NULL UNIQUE
        REFERENCES protocol_runtime_events(id) ON DELETE RESTRICT,
    parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('completed_early', 'crash_failed')),
    state TEXT NOT NULL
        CHECK(state IN ('pending', 'superseded', 'released', 'discarded')),
    resolved_by_terminal_event_id TEXT
        REFERENCES protocol_runtime_events(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    resolved_at_ms INTEGER,
    PRIMARY KEY(agent_session_id, agent_turn_id),
    CHECK(agent_session_id <> parent_session_id),
    CHECK(
        (state = 'pending'
         AND resolved_by_terminal_event_id IS NULL
         AND resolved_at_ms IS NULL)
        OR
        (state IN ('superseded', 'released', 'discarded')
         AND resolved_by_terminal_event_id IS NOT NULL
         AND resolved_at_ms IS NOT NULL)
    ),
    CHECK(updated_at_ms >= created_at_ms),
    CHECK(resolved_at_ms IS NULL OR resolved_at_ms >= created_at_ms)
);

CREATE UNIQUE INDEX idx_agent_deferred_completion_pending
    ON agent_deferred_completions(agent_session_id)
    WHERE state = 'pending';

CREATE INDEX idx_agent_deferred_completion_resolution
    ON agent_deferred_completions(resolved_by_terminal_event_id, agent_session_id)
    WHERE state IN ('superseded', 'released', 'discarded');

CREATE TRIGGER validate_agent_deferred_completion_before_insert
BEFORE INSERT ON agent_deferred_completions
WHEN NOT (
    NEW.state = 'pending'
    AND EXISTS (
        SELECT 1
        FROM session_spawn_edges AS edge
        INNER JOIN protocol_runtime_events AS terminal
          ON terminal.id = NEW.terminal_event_id
         AND terminal.session_id = NEW.agent_session_id
         AND terminal.turn_id = NEW.agent_turn_id
         AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
        WHERE edge.child_session_id = NEW.agent_session_id
          AND edge.parent_session_id = NEW.parent_session_id
          AND (
              (NEW.kind = 'completed_early'
               AND json_extract(
                     terminal.msg_json,
                     '$.terminal.outcome.kind'
                   ) = 'completed')
              OR
              (NEW.kind = 'crash_failed'
               AND json_extract(
                     terminal.msg_json,
                     '$.terminal.outcome.kind'
                   ) = 'failed')
          )
    )
    AND NOT EXISTS (
        SELECT 1
        FROM agent_completion_handoffs AS handoff
        WHERE handoff.child_session_id = NEW.agent_session_id
          AND handoff.child_turn_id = NEW.agent_turn_id
    )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'deferred completion must link an unhanded non-root terminal to its immediate parent'
    );
END;

CREATE TRIGGER validate_agent_deferred_completion_before_update
BEFORE UPDATE ON agent_deferred_completions
WHEN
    NEW.agent_session_id <> OLD.agent_session_id
    OR NEW.agent_turn_id <> OLD.agent_turn_id
    OR NEW.terminal_event_id <> OLD.terminal_event_id
    OR NEW.parent_session_id <> OLD.parent_session_id
    OR NEW.kind <> OLD.kind
    OR NEW.created_at_ms <> OLD.created_at_ms
    OR OLD.state <> 'pending'
    OR NEW.state NOT IN ('superseded', 'released', 'discarded')
    OR NOT (
        (
            NEW.state = 'superseded'
            AND (
                EXISTS (
                    SELECT 1
                    FROM protocol_runtime_events AS resolver
                    INNER JOIN session_spawn_edges AS child_edge
                      ON child_edge.child_session_id = resolver.session_id
                     AND child_edge.parent_session_id = NEW.agent_session_id
                    INNER JOIN agent_completion_handoffs AS handoff
                      ON handoff.child_session_id = resolver.session_id
                     AND handoff.child_turn_id = resolver.turn_id
                     AND handoff.parent_session_id = NEW.agent_session_id
                    WHERE resolver.id = NEW.resolved_by_terminal_event_id
                      AND json_extract(resolver.msg_json, '$.kind')
                          = 'turn_terminal'
                      AND json_extract(
                            resolver.msg_json,
                            '$.terminal.outcome.kind'
                          ) IN ('completed', 'failed')
                )
                OR
                (
                    OLD.kind = 'crash_failed'
                    AND EXISTS (
                        SELECT 1
                        FROM protocol_runtime_events AS resolver
                        INNER JOIN protocol_item_append_order AS resolver_order
                          ON resolver_order.session_id = resolver.session_id
                         AND resolver_order.source_kind = 'runtime_event'
                         AND resolver_order.source_id = resolver.id
                        INNER JOIN protocol_item_append_order AS deferred_order
                          ON deferred_order.session_id = OLD.agent_session_id
                         AND deferred_order.source_kind = 'runtime_event'
                         AND deferred_order.source_id = OLD.terminal_event_id
                        WHERE resolver.id = NEW.resolved_by_terminal_event_id
                          AND resolver.session_id = OLD.agent_session_id
                          AND resolver.turn_id <> OLD.agent_turn_id
                          AND json_extract(resolver.msg_json, '$.kind')
                              = 'turn_terminal'
                          AND json_extract(
                                resolver.msg_json,
                                '$.terminal.outcome.kind'
                              ) IN ('completed', 'failed')
                          AND resolver_order.append_position >
                              deferred_order.append_position
                    )
                )
            )
        )
        OR
        (
            NEW.state = 'released'
            AND EXISTS (
                SELECT 1
                FROM agent_completion_handoffs AS handoff
                WHERE handoff.child_session_id = NEW.agent_session_id
                  AND handoff.child_turn_id = NEW.agent_turn_id
            )
            AND EXISTS (
                WITH RECURSIVE descendants(session_id) AS (
                    SELECT edge.child_session_id
                    FROM session_spawn_edges AS edge
                    WHERE edge.parent_session_id = NEW.agent_session_id
                    UNION ALL
                    SELECT edge.child_session_id
                    FROM session_spawn_edges AS edge
                    INNER JOIN descendants AS parent
                      ON edge.parent_session_id = parent.session_id
                )
                SELECT 1
                FROM descendants
                INNER JOIN protocol_runtime_events AS resolver
                  ON resolver.session_id = descendants.session_id
                 AND resolver.id = NEW.resolved_by_terminal_event_id
                INNER JOIN protocol_item_append_order AS resolver_order
                  ON resolver_order.session_id = resolver.session_id
                 AND resolver_order.source_kind = 'runtime_event'
                 AND resolver_order.source_id = resolver.id
                INNER JOIN protocol_item_append_order AS deferred_order
                  ON deferred_order.session_id = NEW.agent_session_id
                 AND deferred_order.source_kind = 'runtime_event'
                 AND deferred_order.source_id = NEW.terminal_event_id
                 AND json_extract(resolver.msg_json, '$.kind') = 'turn_terminal'
                 AND json_extract(
                       resolver.msg_json,
                       '$.terminal.outcome.kind'
                     ) = 'interrupted'
                 AND json_extract(
                       resolver.msg_json,
                       '$.terminal.outcome.cause'
                     ) = 'agent_interrupted'
                 AND resolver_order.append_position >
                     deferred_order.append_position
            )
            AND NOT EXISTS (
                WITH RECURSIVE descendants(session_id) AS (
                    SELECT edge.child_session_id
                    FROM session_spawn_edges AS edge
                    WHERE edge.parent_session_id = NEW.agent_session_id
                    UNION ALL
                    SELECT edge.child_session_id
                    FROM session_spawn_edges AS edge
                    INNER JOIN descendants AS parent
                      ON edge.parent_session_id = parent.session_id
                )
                SELECT 1
                FROM descendants
                INNER JOIN sessions AS descendant
                  ON descendant.id = descendants.session_id
                WHERE descendant.status = 'running'
                   OR descendant.active_run_id IS NOT NULL
                   OR EXISTS (
                       SELECT 1
                       FROM agent_owner_resume_requests AS resume
                       WHERE resume.owner_session_id = descendant.id
                         AND resume.state IN ('pending', 'claimed')
                   )
                   OR EXISTS (
                       SELECT 1
                       FROM agent_deferred_completions AS deferred
                       WHERE deferred.agent_session_id = descendant.id
                         AND deferred.state = 'pending'
                   )
                   OR EXISTS (
                       SELECT 1
                       FROM protocol_history_items AS trigger_history
                       INNER JOIN protocol_item_append_order AS trigger_order
                         ON trigger_order.session_id =
                            trigger_history.session_id
                        AND trigger_order.source_kind = 'history_item'
                        AND trigger_order.source_id = trigger_history.id
                       WHERE trigger_history.session_id = descendant.id
                         AND trigger_history.scope_kind = 'session'
                         AND json_extract(
                               trigger_history.payload_json,
                               '$.kind'
                             ) = 'inter_agent_communication'
                         AND json_extract(
                               trigger_history.payload_json,
                               '$.communication.trigger_turn'
                             ) = 1
                         AND NOT EXISTS (
                             SELECT 1
                             FROM protocol_item_append_order AS claimed_order
                             WHERE claimed_order.session_id =
                                   trigger_history.session_id
                               AND claimed_order.scope_kind = 'turn'
                               AND claimed_order.append_position >
                                   trigger_order.append_position
                         )
                   )
            )
        )
        OR
        (
            NEW.state = 'discarded'
            AND NOT EXISTS (
                SELECT 1
                FROM agent_completion_handoffs AS handoff
                WHERE handoff.child_session_id = NEW.agent_session_id
                  AND handoff.child_turn_id = NEW.agent_turn_id
            )
            AND (
                EXISTS (
                    WITH RECURSIVE descendants(session_id) AS (
                        SELECT edge.child_session_id
                        FROM session_spawn_edges AS edge
                        WHERE edge.parent_session_id = NEW.agent_session_id
                        UNION ALL
                        SELECT edge.child_session_id
                        FROM session_spawn_edges AS edge
                        INNER JOIN descendants AS parent
                          ON edge.parent_session_id = parent.session_id
                    )
                    SELECT 1
                    FROM descendants
                    INNER JOIN protocol_runtime_events AS resolver
                      ON resolver.session_id = descendants.session_id
                     AND resolver.id = NEW.resolved_by_terminal_event_id
                    INNER JOIN protocol_item_append_order AS resolver_order
                      ON resolver_order.session_id = resolver.session_id
                     AND resolver_order.source_kind = 'runtime_event'
                     AND resolver_order.source_id = resolver.id
                    INNER JOIN protocol_item_append_order AS deferred_order
                      ON deferred_order.session_id = NEW.agent_session_id
                     AND deferred_order.source_kind = 'runtime_event'
                     AND deferred_order.source_id = NEW.terminal_event_id
                     AND json_extract(resolver.msg_json, '$.kind')
                         = 'turn_terminal'
                     AND json_extract(
                           resolver.msg_json,
                           '$.terminal.outcome.kind'
                         ) = 'interrupted'
                     AND json_extract(
                           resolver.msg_json,
                           '$.terminal.outcome.cause'
                         ) IN ('approval_aborted', 'user_stop', 'tree_stopped')
                     AND resolver_order.append_position >
                         deferred_order.append_position
                )
                OR
                (
                    EXISTS (
                        SELECT 1
                        FROM protocol_runtime_events AS resolver
                        INNER JOIN protocol_item_append_order AS resolver_order
                          ON resolver_order.session_id = resolver.session_id
                         AND resolver_order.source_kind = 'runtime_event'
                         AND resolver_order.source_id = resolver.id
                        INNER JOIN protocol_item_append_order AS deferred_order
                          ON deferred_order.session_id = OLD.agent_session_id
                         AND deferred_order.source_kind = 'runtime_event'
                         AND deferred_order.source_id = OLD.terminal_event_id
                        WHERE resolver.id = NEW.resolved_by_terminal_event_id
                          AND resolver.session_id = OLD.agent_session_id
                          AND resolver.turn_id <> OLD.agent_turn_id
                          AND json_extract(resolver.msg_json, '$.kind')
                              = 'turn_terminal'
                          AND json_extract(
                                resolver.msg_json,
                                '$.terminal.outcome.kind'
                              ) = 'interrupted'
                          AND json_extract(
                                resolver.msg_json,
                                '$.terminal.outcome.cause'
                              ) IN (
                                  'approval_aborted',
                                  'user_stop',
                                  'tree_stopped'
                              )
                          AND resolver_order.append_position >
                              deferred_order.append_position
                    )
                )
                OR
                (
                    OLD.kind = 'crash_failed'
                    AND EXISTS (
                        SELECT 1
                        FROM protocol_runtime_events AS resolver
                        INNER JOIN protocol_item_append_order AS resolver_order
                          ON resolver_order.session_id = resolver.session_id
                         AND resolver_order.source_kind = 'runtime_event'
                         AND resolver_order.source_id = resolver.id
                        INNER JOIN protocol_item_append_order AS deferred_order
                          ON deferred_order.session_id = OLD.agent_session_id
                         AND deferred_order.source_kind = 'runtime_event'
                         AND deferred_order.source_id = OLD.terminal_event_id
                        WHERE resolver.id = NEW.resolved_by_terminal_event_id
                          AND resolver.session_id = OLD.agent_session_id
                          AND resolver.turn_id <> OLD.agent_turn_id
                          AND json_extract(resolver.msg_json, '$.kind')
                              = 'turn_terminal'
                          AND json_extract(
                                resolver.msg_json,
                                '$.terminal.outcome.kind'
                              ) = 'interrupted'
                          AND resolver_order.append_position >
                              deferred_order.append_position
                    )
                )
            )
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'invalid deferred completion resolution');
END;

-- V47 had no deferred-completion receipt. Preserve an already-terminal
-- non-root owner without inventing a FINAL when durable descendant work still
-- exists. The terminal becomes releasable or supersedable under the V48 rules.
INSERT OR IGNORE INTO agent_deferred_completions (
    agent_session_id,
    agent_turn_id,
    terminal_event_id,
    parent_session_id,
    kind,
    state,
    resolved_by_terminal_event_id,
    created_at_ms,
    updated_at_ms,
    resolved_at_ms
)
SELECT
    terminal.session_id,
    terminal.turn_id,
    terminal.id,
    edge.parent_session_id,
    CASE json_extract(terminal.msg_json, '$.terminal.outcome.kind')
        WHEN 'completed' THEN 'completed_early'
        ELSE 'crash_failed'
    END,
    'pending',
    NULL,
    terminal.created_at_ms,
    terminal.created_at_ms,
    NULL
FROM protocol_runtime_events AS terminal
INNER JOIN session_spawn_edges AS edge
  ON edge.child_session_id = terminal.session_id
WHERE json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
  AND json_extract(terminal.msg_json, '$.terminal.outcome.kind')
      IN ('completed', 'failed')
  AND NOT EXISTS (
      SELECT 1
      FROM agent_completion_handoffs AS handoff
      WHERE handoff.child_session_id = terminal.session_id
        AND handoff.child_turn_id = terminal.turn_id
  )
  AND NOT EXISTS (
      SELECT 1
      FROM protocol_runtime_events AS later_terminal
      INNER JOIN protocol_item_append_order AS later_order
        ON later_order.session_id = later_terminal.session_id
       AND later_order.source_kind = 'runtime_event'
       AND later_order.source_id = later_terminal.id
      INNER JOIN protocol_item_append_order AS terminal_order
        ON terminal_order.session_id = terminal.session_id
       AND terminal_order.source_kind = 'runtime_event'
       AND terminal_order.source_id = terminal.id
      WHERE later_terminal.session_id = terminal.session_id
        AND json_extract(later_terminal.msg_json, '$.kind') = 'turn_terminal'
        AND later_order.append_position > terminal_order.append_position
  )
  AND EXISTS (
      WITH RECURSIVE descendants(session_id) AS (
          SELECT child.child_session_id
          FROM session_spawn_edges AS child
          WHERE child.parent_session_id = terminal.session_id
          UNION ALL
          SELECT child.child_session_id
          FROM session_spawn_edges AS child
          INNER JOIN descendants AS parent
            ON child.parent_session_id = parent.session_id
      )
      SELECT 1
      FROM descendants
      INNER JOIN sessions AS descendant ON descendant.id = descendants.session_id
      WHERE descendant.status = 'running'
         OR descendant.active_run_id IS NOT NULL
         OR EXISTS (
             SELECT 1
             FROM agent_owner_resume_requests AS resume
             WHERE resume.owner_session_id = descendant.id
               AND resume.state IN ('pending', 'claimed')
         )
         OR EXISTS (
             SELECT 1
             FROM protocol_history_items AS trigger_history
             INNER JOIN protocol_item_append_order AS trigger_order
               ON trigger_order.session_id = trigger_history.session_id
              AND trigger_order.source_kind = 'history_item'
              AND trigger_order.source_id = trigger_history.id
             WHERE trigger_history.session_id = descendant.id
               AND trigger_history.scope_kind = 'session'
               AND json_extract(trigger_history.payload_json, '$.kind')
                   = 'inter_agent_communication'
               AND json_extract(
                     trigger_history.payload_json,
                     '$.communication.trigger_turn'
                   ) = 1
               AND NOT EXISTS (
                   SELECT 1
                   FROM protocol_item_append_order AS claimed_order
                   WHERE claimed_order.session_id = trigger_history.session_id
                     AND claimed_order.scope_kind = 'turn'
                     AND claimed_order.append_position > trigger_order.append_position
               )
         )
  );

-- Existing V47 session-scoped direct-child FINAL handoffs become resumable for
-- an inactive non-root owner. Root consumes its mailbox in its existing loop.
INSERT OR IGNORE INTO agent_owner_resume_requests (
    owner_session_id,
    source_session_id,
    source_history_item_id,
    state,
    claimed_turn_id,
    created_at_ms,
    updated_at_ms,
    claimed_at_ms,
    resolved_at_ms
)
SELECT
    handoff.parent_session_id,
    handoff.parent_session_id,
    handoff.parent_history_item_id,
    'pending',
    NULL,
    handoff.created_at_ms,
    handoff.created_at_ms,
    NULL,
    NULL
FROM agent_completion_handoffs AS handoff
INNER JOIN protocol_history_items AS history
  ON history.id = handoff.parent_history_item_id
 AND history.session_id = handoff.parent_session_id
 AND history.scope_kind = 'session'
INNER JOIN protocol_item_append_order AS result_order
  ON result_order.session_id = history.session_id
 AND result_order.source_kind = 'history_item'
 AND result_order.source_id = history.id
INNER JOIN session_spawn_edges AS owner_edge
  ON owner_edge.child_session_id = handoff.parent_session_id
INNER JOIN sessions AS owner
  ON owner.id = handoff.parent_session_id
WHERE (
        owner.status IN ('idle', 'completed')
        OR EXISTS (
            SELECT 1
            FROM agent_deferred_completions AS deferred
            WHERE deferred.agent_session_id = handoff.parent_session_id
              AND deferred.state = 'pending'
              AND deferred.kind = 'crash_failed'
        )
      )
  AND NOT EXISTS (
      SELECT 1
      FROM protocol_item_append_order AS claimed_order
      WHERE claimed_order.session_id = history.session_id
        AND claimed_order.scope_kind = 'turn'
        AND claimed_order.append_position > result_order.append_position
  );

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (48, 'agent_owner_resume_requests');
