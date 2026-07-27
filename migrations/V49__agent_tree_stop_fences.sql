CREATE TABLE agent_tree_stop_fences (
    root_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    stopped_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    after_append_position INTEGER NOT NULL CHECK(after_append_position >= 0),
    cause TEXT NOT NULL
        CHECK(cause IN (
            'approval_aborted',
            'user_stop',
            'tree_stopped',
            'root_failed'
        )),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    PRIMARY KEY(stopped_session_id, after_append_position)
);

CREATE INDEX idx_agent_tree_stop_fences_root_position
    ON agent_tree_stop_fences(
        root_session_id,
        after_append_position,
        stopped_session_id
    );

CREATE TRIGGER validate_agent_tree_stop_fence_before_insert
BEFORE INSERT ON agent_tree_stop_fences
WHEN NOT EXISTS (
    SELECT 1
    FROM sessions AS root
    INNER JOIN sessions AS stopped
      ON stopped.id = NEW.stopped_session_id
     AND stopped.project_id = root.project_id
    WHERE root.id = NEW.root_session_id
)
BEGIN
    SELECT RAISE(
        ABORT,
        'tree stop fence sessions must exist in one project'
    );
END;

CREATE TRIGGER validate_agent_tree_stop_fence_root_before_insert
BEFORE INSERT ON agent_tree_stop_fences
WHEN EXISTS (
    SELECT 1
    FROM session_spawn_edges AS owner
    WHERE owner.child_session_id = NEW.root_session_id
)
BEGIN
    SELECT RAISE(
        ABORT,
        'tree stop fence root must be the canonical tree root'
    );
END;

CREATE TRIGGER validate_agent_tree_stop_fence_subtree_before_insert
BEFORE INSERT ON agent_tree_stop_fences
WHEN
    NEW.stopped_session_id <> NEW.root_session_id
    AND NOT EXISTS (
        SELECT 1
        FROM session_spawn_edges AS stopped_edge
        WHERE stopped_edge.root_session_id = NEW.root_session_id
          AND stopped_edge.child_session_id = NEW.stopped_session_id
    )
BEGIN
    SELECT RAISE(
        ABORT,
        'tree stop fence stopped session must belong to the canonical tree'
    );
END;

CREATE TRIGGER validate_agent_tree_stop_fence_boundary_before_insert
BEFORE INSERT ON agent_tree_stop_fences
WHEN NEW.after_append_position > MAX(
    COALESCE(
        (SELECT MAX(append_position) FROM protocol_item_append_order),
        0
    ),
    COALESCE(
        (
            SELECT seq
            FROM sqlite_sequence
            WHERE name = 'protocol_item_append_order'
        ),
        0
    )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'tree stop fence append boundary must already be committed'
    );
END;

CREATE TRIGGER validate_agent_tree_stop_fence_cause_before_insert
BEFORE INSERT ON agent_tree_stop_fences
WHEN
    (
        NEW.cause = 'root_failed'
        AND NEW.stopped_session_id <> NEW.root_session_id
    )
    OR
    (
        NEW.cause = 'tree_stopped'
        AND (
            WITH RECURSIVE origin_scope(
                root_session_id,
                stopped_session_id,
                after_append_position,
                session_id
            ) AS (
                SELECT
                    fence.root_session_id,
                    fence.stopped_session_id,
                    fence.after_append_position,
                    fence.stopped_session_id
                FROM agent_tree_stop_fences AS fence
                WHERE fence.root_session_id = NEW.root_session_id
                  AND fence.after_append_position =
                      NEW.after_append_position
                  AND fence.cause IN (
                      'approval_aborted',
                      'user_stop',
                      'root_failed'
                  )
                UNION ALL
                SELECT
                    origin.root_session_id,
                    origin.stopped_session_id,
                    origin.after_append_position,
                    child.child_session_id
                FROM origin_scope AS origin
                INNER JOIN session_spawn_edges AS child
                  ON child.root_session_id = origin.root_session_id
                 AND child.parent_session_id = origin.session_id
            )
            SELECT COUNT(*)
            FROM origin_scope
            WHERE session_id = NEW.stopped_session_id
        ) <> 1
    )
BEGIN
    SELECT RAISE(
        ABORT,
        'tree-stopped fences require one earlier ancestor origin at the same boundary'
    );
END;

CREATE TRIGGER prevent_agent_tree_stop_fence_update
BEFORE UPDATE ON agent_tree_stop_fences
BEGIN
    SELECT RAISE(ABORT, 'agent tree stop fences are immutable');
END;

-- V48 had no durable tree-close generation. Recover exact destructive stops
-- and canonical-root failures from their terminal append boundary.
INSERT OR IGNORE INTO agent_tree_stop_fences (
    root_session_id,
    stopped_session_id,
    after_append_position,
    cause,
    created_at_ms
)
SELECT
    COALESCE(edge.root_session_id, terminal.session_id),
    terminal.session_id,
    terminal_order.append_position,
    CASE json_extract(terminal.msg_json, '$.terminal.outcome.kind')
        WHEN 'failed' THEN 'root_failed'
        ELSE json_extract(terminal.msg_json, '$.terminal.outcome.cause')
    END,
    terminal.created_at_ms
FROM protocol_runtime_events AS terminal
INNER JOIN protocol_item_append_order AS terminal_order
  ON terminal_order.session_id = terminal.session_id
 AND terminal_order.source_kind = 'runtime_event'
 AND terminal_order.source_id = terminal.id
LEFT JOIN session_spawn_edges AS edge
  ON edge.child_session_id = terminal.session_id
INNER JOIN sessions AS stopped
  ON stopped.id = terminal.session_id
INNER JOIN sessions AS root
  ON root.id = COALESCE(edge.root_session_id, terminal.session_id)
 AND root.project_id = stopped.project_id
WHERE json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
  AND NOT EXISTS (
      SELECT 1
      FROM session_spawn_edges AS owner
      WHERE owner.child_session_id = root.id
  )
  AND (
      (
          json_extract(terminal.msg_json, '$.terminal.outcome.kind')
              = 'interrupted'
          AND json_extract(
                terminal.msg_json,
                '$.terminal.outcome.cause'
              ) IN ('approval_aborted', 'user_stop')
      )
      OR
      (
          json_extract(terminal.msg_json, '$.terminal.outcome.kind') = 'failed'
          AND edge.child_session_id IS NULL
      )
  );

-- A descendant TreeStopped terminal does not own a new generation boundary.
-- Bind it to exactly one earlier ancestor stop only when the stopped turn was
-- already active at that boundary. Ambiguous and post-stop follow-up terminals
-- intentionally produce no standalone child fence.
WITH RECURSIVE fenced_scope(
    root_session_id,
    stopped_session_id,
    after_append_position,
    session_id
) AS (
    SELECT
        fence.root_session_id,
        fence.stopped_session_id,
        fence.after_append_position,
        fence.stopped_session_id
    FROM agent_tree_stop_fences AS fence
    WHERE fence.cause IN ('approval_aborted', 'user_stop', 'root_failed')
    UNION ALL
    SELECT
        fenced.root_session_id,
        fenced.stopped_session_id,
        fenced.after_append_position,
        child.child_session_id
    FROM fenced_scope AS fenced
    INNER JOIN session_spawn_edges AS child
      ON child.root_session_id = fenced.root_session_id
     AND child.parent_session_id = fenced.session_id
),
eligible_tree_stops AS (
    SELECT
        terminal.id AS terminal_event_id,
        terminal.session_id,
        terminal.created_at_ms,
        fenced.root_session_id,
        fenced.after_append_position
    FROM protocol_runtime_events AS terminal
    INNER JOIN protocol_item_append_order AS terminal_order
      ON terminal_order.session_id = terminal.session_id
     AND terminal_order.source_kind = 'runtime_event'
     AND terminal_order.source_id = terminal.id
    INNER JOIN session_spawn_edges AS terminal_edge
      ON terminal_edge.child_session_id = terminal.session_id
    INNER JOIN fenced_scope AS fenced
      ON fenced.root_session_id = terminal_edge.root_session_id
     AND fenced.session_id = terminal.session_id
    WHERE json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
      AND json_extract(terminal.msg_json, '$.terminal.outcome.kind')
          = 'interrupted'
      AND json_extract(terminal.msg_json, '$.terminal.outcome.cause')
          = 'tree_stopped'
      AND terminal_order.append_position > fenced.after_append_position
      AND (
          SELECT MIN(turn_order.append_position)
          FROM protocol_item_append_order AS turn_order
          WHERE turn_order.session_id = terminal.session_id
            AND turn_order.turn_id = terminal.turn_id
      ) <= fenced.after_append_position
),
unambiguous_tree_stops AS (
    SELECT
        terminal_event_id,
        session_id,
        MIN(created_at_ms) AS created_at_ms,
        MIN(root_session_id) AS root_session_id,
        MIN(after_append_position) AS after_append_position
    FROM eligible_tree_stops
    GROUP BY terminal_event_id, session_id
    HAVING COUNT(*) = 1
)
INSERT OR IGNORE INTO agent_tree_stop_fences (
    root_session_id,
    stopped_session_id,
    after_append_position,
    cause,
    created_at_ms
)
SELECT
    root_session_id,
    session_id,
    after_append_position,
    'tree_stopped',
    created_at_ms
FROM unambiguous_tree_stops;

CREATE VIEW effective_agent_deferred_completions AS
SELECT
    deferred.agent_session_id,
    deferred.agent_turn_id,
    deferred.terminal_event_id,
    deferred.parent_session_id,
    deferred.kind,
    deferred.state,
    deferred.resolved_by_terminal_event_id,
    deferred.created_at_ms,
    deferred.updated_at_ms,
    deferred.resolved_at_ms
FROM agent_deferred_completions AS deferred
WHERE deferred.state <> 'pending'
   OR NOT EXISTS (
       WITH RECURSIVE
       fenced_scope(
           root_session_id,
           stopped_session_id,
           after_append_position,
           session_id
       ) AS (
           SELECT
               fence.root_session_id,
               fence.stopped_session_id,
               fence.after_append_position,
               fence.stopped_session_id
           FROM agent_tree_stop_fences AS fence
           UNION ALL
           SELECT
               fenced.root_session_id,
               fenced.stopped_session_id,
               fenced.after_append_position,
               child.child_session_id
           FROM fenced_scope AS fenced
           INNER JOIN session_spawn_edges AS child
             ON child.root_session_id = fenced.root_session_id
            AND child.parent_session_id = fenced.session_id
       ),
       deferred_scope(session_id) AS (
           SELECT deferred.agent_session_id
           UNION ALL
           SELECT child.child_session_id
           FROM session_spawn_edges AS child
           INNER JOIN deferred_scope AS owner
             ON child.parent_session_id = owner.session_id
       )
       SELECT 1
       FROM fenced_scope AS fenced
       WHERE (
           SELECT MIN(turn_order.append_position)
           FROM protocol_item_append_order AS turn_order
           WHERE turn_order.session_id = deferred.agent_session_id
             AND turn_order.turn_id = deferred.agent_turn_id
       ) <= fenced.after_append_position
         AND (
             fenced.session_id = deferred.agent_session_id
             OR EXISTS (
                 SELECT 1
                 FROM deferred_scope AS descendant
                 WHERE descendant.session_id = fenced.stopped_session_id
             )
         )
   );

-- A V48 upgrade can seed a wake before V49 recovers an older stop. Cancel
-- work which was already inside the stopped generation, while preserving an
-- explicit reuse whose turn began after the fence.
UPDATE agent_owner_resume_requests AS request
SET state = 'cancelled',
    updated_at_ms = MAX(
        request.updated_at_ms,
        COALESCE(request.claimed_at_ms, 0)
    ),
    resolved_at_ms = MAX(
        request.created_at_ms,
        request.updated_at_ms,
        COALESCE(request.claimed_at_ms, 0)
    )
WHERE request.state IN ('pending', 'claimed')
  AND EXISTS (
      WITH RECURSIVE fenced_scope(
          root_session_id,
          after_append_position,
          session_id
      ) AS (
          SELECT
              fence.root_session_id,
              fence.after_append_position,
              fence.stopped_session_id
          FROM agent_tree_stop_fences AS fence
          UNION ALL
          SELECT
              fenced.root_session_id,
              fenced.after_append_position,
              child.child_session_id
          FROM fenced_scope AS fenced
          INNER JOIN session_spawn_edges AS child
            ON child.root_session_id = fenced.root_session_id
           AND child.parent_session_id = fenced.session_id
      )
      SELECT 1
      FROM agent_completion_handoffs AS handoff
      INNER JOIN protocol_runtime_events AS source_terminal
        ON source_terminal.id = handoff.child_terminal_event_id
      INNER JOIN fenced_scope AS fenced
        ON fenced.session_id = source_terminal.session_id
      WHERE handoff.parent_history_item_id =
            request.source_history_item_id
        AND (
            SELECT MIN(turn_order.append_position)
            FROM protocol_item_append_order AS turn_order
            WHERE turn_order.session_id = source_terminal.session_id
              AND turn_order.turn_id = source_terminal.turn_id
        ) <= fenced.after_append_position
  );

DROP TRIGGER validate_agent_deferred_completion_before_update;

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
                       FROM effective_agent_deferred_completions AS deferred
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
                         AND NOT EXISTS (
                             WITH RECURSIVE fenced_scope(
                                 root_session_id,
                                 after_append_position,
                                 session_id
                             ) AS (
                                 SELECT
                                     fence.root_session_id,
                                     fence.after_append_position,
                                     fence.stopped_session_id
                                 FROM agent_tree_stop_fences AS fence
                                 UNION ALL
                                 SELECT
                                     fenced.root_session_id,
                                     fenced.after_append_position,
                                     child.child_session_id
                                 FROM fenced_scope AS fenced
                                 INNER JOIN session_spawn_edges AS child
                                   ON child.root_session_id =
                                      fenced.root_session_id
                                  AND child.parent_session_id =
                                      fenced.session_id
                             )
                             SELECT 1
                             FROM fenced_scope AS fenced
                             WHERE fenced.session_id =
                                   trigger_history.session_id
                               AND trigger_order.append_position <=
                                   fenced.after_append_position
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
                OR
                (
                    NEW.resolved_by_terminal_event_id = OLD.terminal_event_id
                    AND EXISTS (
                        WITH RECURSIVE
                        fenced_scope(
                            root_session_id,
                            stopped_session_id,
                            after_append_position,
                            session_id
                        ) AS (
                            SELECT
                                fence.root_session_id,
                                fence.stopped_session_id,
                                fence.after_append_position,
                                fence.stopped_session_id
                            FROM agent_tree_stop_fences AS fence
                            UNION ALL
                            SELECT
                                fenced.root_session_id,
                                fenced.stopped_session_id,
                                fenced.after_append_position,
                                child.child_session_id
                            FROM fenced_scope AS fenced
                            INNER JOIN session_spawn_edges AS child
                              ON child.root_session_id =
                                 fenced.root_session_id
                             AND child.parent_session_id =
                                 fenced.session_id
                        ),
                        deferred_scope(session_id) AS (
                            SELECT OLD.agent_session_id
                            UNION ALL
                            SELECT child.child_session_id
                            FROM session_spawn_edges AS child
                            INNER JOIN deferred_scope AS owner
                              ON child.parent_session_id = owner.session_id
                        )
                        SELECT 1
                        FROM protocol_item_append_order AS terminal_order
                        INNER JOIN fenced_scope AS fenced
                          ON (
                              fenced.session_id = OLD.agent_session_id
                              OR EXISTS (
                                  SELECT 1
                                  FROM deferred_scope AS descendant
                                  WHERE descendant.session_id =
                                        fenced.stopped_session_id
                              )
                          )
                        WHERE terminal_order.session_id =
                              OLD.agent_session_id
                          AND terminal_order.source_kind = 'runtime_event'
                          AND terminal_order.source_id =
                              OLD.terminal_event_id
                          AND (
                              SELECT MIN(turn_order.append_position)
                              FROM protocol_item_append_order AS turn_order
                              WHERE turn_order.session_id =
                                    OLD.agent_session_id
                                AND turn_order.turn_id = OLD.agent_turn_id
                          ) <= fenced.after_append_position
                          AND terminal_order.append_position <=
                              fenced.after_append_position
                    )
                )
                OR
                EXISTS (
                    WITH RECURSIVE
                    owner_scope(session_id) AS (
                        SELECT NEW.agent_session_id
                        UNION ALL
                        SELECT edge.child_session_id
                        FROM session_spawn_edges AS edge
                        INNER JOIN owner_scope AS owner
                          ON edge.parent_session_id = owner.session_id
                    ),
                    fenced_scope(
                        root_session_id,
                        after_append_position,
                        session_id
                    ) AS (
                        SELECT
                            fence.root_session_id,
                            fence.after_append_position,
                            fence.stopped_session_id
                        FROM agent_tree_stop_fences AS fence
                        UNION ALL
                        SELECT
                            fenced.root_session_id,
                            fenced.after_append_position,
                            edge.child_session_id
                        FROM fenced_scope AS fenced
                        INNER JOIN session_spawn_edges AS edge
                          ON edge.root_session_id = fenced.root_session_id
                         AND edge.parent_session_id = fenced.session_id
                    )
                    SELECT 1
                    FROM protocol_runtime_events AS resolver
                    INNER JOIN protocol_item_append_order AS resolver_order
                      ON resolver_order.session_id = resolver.session_id
                     AND resolver_order.source_kind = 'runtime_event'
                     AND resolver_order.source_id = resolver.id
                    INNER JOIN owner_scope AS owner
                      ON owner.session_id = resolver.session_id
                    INNER JOIN fenced_scope AS fenced
                      ON fenced.session_id = resolver.session_id
                    WHERE resolver.id = NEW.resolved_by_terminal_event_id
                      AND json_extract(resolver.msg_json, '$.kind')
                          = 'turn_terminal'
                      AND (
                          SELECT MIN(turn_order.append_position)
                          FROM protocol_item_append_order AS turn_order
                          WHERE turn_order.session_id =
                                NEW.agent_session_id
                            AND turn_order.turn_id = NEW.agent_turn_id
                      ) <= fenced.after_append_position
                      AND resolver_order.append_position >
                          fenced.after_append_position
                )
            )
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'invalid deferred completion resolution');
END;

UPDATE agent_deferred_completions AS deferred
SET state = 'discarded',
    resolved_by_terminal_event_id = deferred.terminal_event_id,
    updated_at_ms = MAX(deferred.updated_at_ms, deferred.created_at_ms),
    resolved_at_ms = MAX(deferred.updated_at_ms, deferred.created_at_ms)
WHERE deferred.state = 'pending'
  AND EXISTS (
      WITH RECURSIVE
      fenced_scope(
          root_session_id,
          stopped_session_id,
          after_append_position,
          session_id
      ) AS (
          SELECT
              fence.root_session_id,
              fence.stopped_session_id,
              fence.after_append_position,
              fence.stopped_session_id
          FROM agent_tree_stop_fences AS fence
          UNION ALL
          SELECT
              fenced.root_session_id,
              fenced.stopped_session_id,
              fenced.after_append_position,
              child.child_session_id
          FROM fenced_scope AS fenced
          INNER JOIN session_spawn_edges AS child
            ON child.root_session_id = fenced.root_session_id
           AND child.parent_session_id = fenced.session_id
      ),
      deferred_scope(session_id) AS (
          SELECT deferred.agent_session_id
          UNION ALL
          SELECT child.child_session_id
          FROM session_spawn_edges AS child
          INNER JOIN deferred_scope AS owner
            ON child.parent_session_id = owner.session_id
      )
      SELECT 1
      FROM protocol_item_append_order AS terminal_order
      INNER JOIN fenced_scope AS fenced
        ON (
            fenced.session_id = deferred.agent_session_id
            OR EXISTS (
                SELECT 1
                FROM deferred_scope AS descendant
                WHERE descendant.session_id = fenced.stopped_session_id
            )
        )
      WHERE terminal_order.session_id = deferred.agent_session_id
        AND terminal_order.source_kind = 'runtime_event'
        AND terminal_order.source_id = deferred.terminal_event_id
        AND (
            SELECT MIN(turn_order.append_position)
            FROM protocol_item_append_order AS turn_order
            WHERE turn_order.session_id = deferred.agent_session_id
              AND turn_order.turn_id = deferred.agent_turn_id
        ) <= fenced.after_append_position
        AND terminal_order.append_position <= fenced.after_append_position
  );

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (49, 'agent_tree_stop_fences');
