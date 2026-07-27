CREATE TABLE session_spawn_edges_v47 (
    root_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    child_session_id TEXT PRIMARY KEY NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_path TEXT NOT NULL,
    task_name TEXT NOT NULL,
    spawn_order INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE(root_session_id, agent_path),
    UNIQUE(root_session_id, spawn_order),
    CHECK(child_session_id <> root_session_id),
    CHECK(spawn_order > 0),
    CHECK(task_name <> ''),
    CHECK(task_name <> 'root'),
    CHECK(task_name NOT GLOB '*[^a-z0-9_]*'),
    CHECK(agent_path GLOB '/root/*'),
    CHECK(agent_path NOT GLOB '*[^/a-z0-9_]*'),
    CHECK(agent_path NOT GLOB '*//*'),
    CHECK(agent_path NOT LIKE '%/')
);

INSERT INTO session_spawn_edges_v47 (
    root_session_id,
    parent_session_id,
    child_session_id,
    agent_path,
    task_name,
    spawn_order,
    created_at_ms
)
SELECT
    root_session_id,
    parent_session_id,
    child_session_id,
    agent_path,
    task_name,
    ROW_NUMBER() OVER (
        PARTITION BY root_session_id
        ORDER BY created_at_ms ASC, child_session_id ASC
    ),
    created_at_ms
FROM session_spawn_edges
ORDER BY created_at_ms ASC, child_session_id ASC;

DROP TABLE session_spawn_edges;
ALTER TABLE session_spawn_edges_v47 RENAME TO session_spawn_edges;

CREATE INDEX idx_session_spawn_edges_root_order
    ON session_spawn_edges(root_session_id, spawn_order, child_session_id);

CREATE INDEX idx_session_spawn_edges_parent_created
    ON session_spawn_edges(parent_session_id, created_at_ms, child_session_id);

CREATE TRIGGER validate_session_spawn_edge_parent_before_insert
BEFORE INSERT ON session_spawn_edges
WHEN NOT (
    EXISTS (
        SELECT 1
        FROM sessions AS root
        INNER JOIN sessions AS parent
            ON parent.id = NEW.parent_session_id
           AND parent.project_id = root.project_id
        INNER JOIN sessions AS child
            ON child.id = NEW.child_session_id
           AND child.project_id = root.project_id
        WHERE root.id = NEW.root_session_id
    )
    AND NOT EXISTS (
        SELECT 1
        FROM session_spawn_edges AS owner
        WHERE owner.child_session_id = NEW.root_session_id
    )
    AND NOT EXISTS (
        SELECT 1
        FROM session_spawn_edges AS owned_tree
        WHERE owned_tree.root_session_id = NEW.child_session_id
    )
    AND
    (
        (
            NEW.parent_session_id = NEW.root_session_id
            AND NEW.agent_path = '/root/' || NEW.task_name
        )
        OR
        (
            NEW.parent_session_id <> NEW.root_session_id
            AND EXISTS (
                SELECT 1
                FROM session_spawn_edges AS parent
                WHERE parent.root_session_id = NEW.root_session_id
                  AND parent.child_session_id = NEW.parent_session_id
                  AND NEW.agent_path = parent.agent_path || '/' || NEW.task_name
            )
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'spawn edge sessions must share one project and one canonical agent-tree owner');
END;

CREATE TRIGGER prevent_session_spawn_edge_update
BEFORE UPDATE ON session_spawn_edges
BEGIN
    SELECT RAISE(ABORT, 'session spawn edges are immutable');
END;

CREATE TRIGGER prevent_session_spawn_edge_orphan_on_delete
BEFORE DELETE ON session_spawn_edges
WHEN EXISTS (
    SELECT 1
    FROM session_spawn_edges AS child
    WHERE child.root_session_id = OLD.root_session_id
      AND child.parent_session_id = OLD.child_session_id
)
BEGIN
    SELECT RAISE(ABORT, 'cannot delete a session spawn edge while it retains descendants');
END;

-- MAX_RETAINED_AGENTS is 256 including the root, leaving at most 255 descendants.
CREATE TRIGGER limit_session_spawn_edges_per_root
BEFORE INSERT ON session_spawn_edges
WHEN (
    SELECT COUNT(*)
    FROM session_spawn_edges
    WHERE root_session_id = NEW.root_session_id
) >= 255
BEGIN
    SELECT RAISE(ABORT, 'agent tree reached its retained-descendant capacity of 255');
END;

CREATE TABLE agent_completion_handoffs (
    child_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    child_turn_id TEXT NOT NULL,
    child_terminal_event_id TEXT NOT NULL UNIQUE
        REFERENCES protocol_runtime_events(id) ON DELETE RESTRICT,
    parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    parent_history_item_id TEXT NOT NULL UNIQUE
        REFERENCES protocol_history_items(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(child_session_id, child_turn_id),
    CHECK(child_session_id <> parent_session_id)
);

CREATE TRIGGER validate_agent_completion_handoff_before_insert
BEFORE INSERT ON agent_completion_handoffs
WHEN NOT EXISTS (
    SELECT 1
    FROM session_spawn_edges AS edge
    INNER JOIN protocol_runtime_events AS terminal
      ON terminal.id = NEW.child_terminal_event_id
     AND terminal.session_id = NEW.child_session_id
     AND terminal.turn_id = NEW.child_turn_id
     AND json_extract(terminal.msg_json, '$.kind') = 'turn_terminal'
     AND json_extract(terminal.msg_json, '$.terminal.outcome.kind')
         IN ('completed', 'failed')
    INNER JOIN protocol_history_items AS history
      ON history.id = NEW.parent_history_item_id
     AND history.session_id = NEW.parent_session_id
     AND json_extract(history.payload_json, '$.kind')
         = 'inter_agent_communication'
     AND json_extract(history.payload_json, '$.communication.author')
         = edge.agent_path
     AND json_extract(history.payload_json, '$.communication.trigger_turn') = 0
    LEFT JOIN session_spawn_edges AS parent_edge
      ON parent_edge.root_session_id = edge.root_session_id
     AND parent_edge.child_session_id = edge.parent_session_id
    WHERE edge.child_session_id = NEW.child_session_id
      AND edge.parent_session_id = NEW.parent_session_id
      AND json_extract(history.payload_json, '$.communication.recipient')
          = CASE
                WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                ELSE parent_edge.agent_path
            END
      AND instr(
              json_extract(history.payload_json, '$.communication.content'),
              'Message Type: FINAL_ANSWER' || char(10)
               || 'Task name: ' ||
               CASE
                   WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                   ELSE parent_edge.agent_path
               END
               || char(10) || 'Sender: ' || edge.agent_path
               || char(10) || 'Payload:' || char(10)
          ) = 1
)
BEGIN
    SELECT RAISE(
        ABORT,
        'agent completion handoff must link one child terminal to its immediate parent FINAL history item'
    );
END;

CREATE TRIGGER prevent_agent_completion_handoff_update
BEFORE UPDATE ON agent_completion_handoffs
BEGIN
    SELECT RAISE(ABORT, 'agent completion handoffs are immutable');
END;

-- V46 committed child terminals and parent FINAL history in separate
-- transactions. Preserve already-complete pairs without inventing a missing
-- user-visible FINAL: eligible terminals and matching immediate-parent FINALs
-- are paired in their durable append order for each child.
WITH ordered_eligible_terminals AS (
    SELECT
        edge.child_session_id,
        runtime.turn_id AS child_turn_id,
        runtime.id AS child_terminal_event_id,
        runtime.created_at_ms,
        append_order.append_position AS terminal_append_position,
        LEAD(append_order.append_position) OVER (
            PARTITION BY edge.child_session_id
            ORDER BY append_order.append_position ASC, runtime.id ASC
        ) AS next_terminal_append_position
    FROM session_spawn_edges AS edge
    INNER JOIN protocol_runtime_events AS runtime
      ON runtime.session_id = edge.child_session_id
     AND json_extract(runtime.msg_json, '$.kind') = 'turn_terminal'
     AND json_extract(runtime.msg_json, '$.terminal.outcome.kind')
         IN ('completed', 'failed')
    INNER JOIN protocol_item_append_order AS append_order
      ON append_order.session_id = runtime.session_id
     AND append_order.source_kind = 'runtime_event'
     AND append_order.source_id = runtime.id
),
existing_parent_finals AS (
    SELECT
        edge.child_session_id,
        edge.parent_session_id,
        history.id AS parent_history_item_id,
        history.created_at_ms,
        append_order.append_position AS final_append_position
    FROM session_spawn_edges AS edge
    INNER JOIN protocol_history_items AS history
      ON history.session_id = edge.parent_session_id
     AND json_extract(history.payload_json, '$.kind')
         = 'inter_agent_communication'
     AND json_extract(history.payload_json, '$.communication.author')
         = edge.agent_path
     AND json_extract(history.payload_json, '$.communication.trigger_turn') = 0
    INNER JOIN protocol_item_append_order AS append_order
      ON append_order.session_id = history.session_id
     AND append_order.source_kind = 'history_item'
     AND append_order.source_id = history.id
    LEFT JOIN session_spawn_edges AS parent_edge
      ON parent_edge.root_session_id = edge.root_session_id
     AND parent_edge.child_session_id = edge.parent_session_id
    WHERE json_extract(history.payload_json, '$.communication.recipient')
          = CASE
                WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                ELSE parent_edge.agent_path
            END
      AND instr(
              json_extract(history.payload_json, '$.communication.content'),
              'Message Type: FINAL_ANSWER' || char(10)
               || 'Task name: ' ||
               CASE
                   WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                   ELSE parent_edge.agent_path
               END
               || char(10) || 'Sender: ' || edge.agent_path
               || char(10) || 'Payload:' || char(10)
          ) = 1
),
unambiguous_pairs AS (
    SELECT
        terminal.child_session_id,
        terminal.child_turn_id,
        terminal.child_terminal_event_id,
        terminal.created_at_ms AS terminal_created_at_ms,
        final.parent_session_id,
        MIN(final.parent_history_item_id) AS parent_history_item_id,
        MAX(final.created_at_ms) AS final_created_at_ms
    FROM ordered_eligible_terminals AS terminal
    INNER JOIN existing_parent_finals AS final
      ON final.child_session_id = terminal.child_session_id
     AND final.final_append_position > terminal.terminal_append_position
     AND (
         terminal.next_terminal_append_position IS NULL
         OR final.final_append_position < terminal.next_terminal_append_position
     )
    GROUP BY
        terminal.child_session_id,
        terminal.child_turn_id,
        terminal.child_terminal_event_id,
        terminal.created_at_ms,
        final.parent_session_id
    HAVING COUNT(*) = 1
)
INSERT OR IGNORE INTO agent_completion_handoffs (
    child_session_id,
    child_turn_id,
    child_terminal_event_id,
    parent_session_id,
    parent_history_item_id,
    created_at_ms
)
SELECT
    pair.child_session_id,
    pair.child_turn_id,
    pair.child_terminal_event_id,
    pair.parent_session_id,
    pair.parent_history_item_id,
    MAX(pair.terminal_created_at_ms, pair.final_created_at_ms)
FROM unambiguous_pairs AS pair;

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (47, 'recursive_session_spawn_edges');
