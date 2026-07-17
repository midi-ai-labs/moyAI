-- History scope is a single typed owner. The physical nullable turn_id is
-- valid only when paired with scope_kind='turn'; session-scoped state never
-- invents a turn identity.

CREATE TEMP TABLE v42_session_history_items (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    old_turn_id TEXT NOT NULL,
    append_position INTEGER NOT NULL,
    new_sequence_no INTEGER NOT NULL
);

WITH session_candidates AS (
    SELECT
        history.id,
        history.session_id,
        history.turn_id AS old_turn_id,
        append_order.append_position
    FROM protocol_history_items AS history
    INNER JOIN protocol_item_append_order AS append_order
      ON append_order.session_id = history.session_id
     AND append_order.source_kind = 'history_item'
     AND append_order.source_id = history.id
    WHERE json_valid(history.payload_json)
      AND json_extract(history.payload_json, '$.kind') = 'collaboration_mode_instruction'
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_history_items AS sibling
          WHERE sibling.session_id = history.session_id
            AND sibling.turn_id = history.turn_id
            AND (
                NOT json_valid(sibling.payload_json)
                OR json_extract(sibling.payload_json, '$.kind') <> 'collaboration_mode_instruction'
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_turn_items AS turn_item
          WHERE turn_item.session_id = history.session_id
            AND turn_item.turn_id = history.turn_id
      )
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_runtime_events AS runtime_event
          WHERE runtime_event.session_id = history.session_id
            AND runtime_event.turn_id = history.turn_id
      )
      AND NOT EXISTS (
          SELECT 1
          FROM sessions AS session
          WHERE session.id = history.session_id
            AND session.active_turn_id = history.turn_id
      )

    UNION

    SELECT
        history.id,
        history.session_id,
        history.turn_id AS old_turn_id,
        append_order.append_position
    FROM protocol_history_items AS history
    INNER JOIN protocol_item_append_order AS append_order
      ON append_order.session_id = history.session_id
     AND append_order.source_kind = 'history_item'
     AND append_order.source_id = history.id
    WHERE json_valid(history.payload_json)
      AND json_extract(history.payload_json, '$.kind') = 'inter_agent_communication'
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_history_items AS sibling
          WHERE sibling.session_id = history.session_id
            AND sibling.turn_id = history.turn_id
            AND (
                NOT json_valid(sibling.payload_json)
                OR json_extract(sibling.payload_json, '$.kind') <> 'inter_agent_communication'
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_turn_items AS turn_item
          WHERE turn_item.session_id = history.session_id
            AND turn_item.turn_id = history.turn_id
            AND (
                NOT json_valid(turn_item.payload_json)
                OR json_extract(turn_item.payload_json, '$.kind') <> 'inter_agent_communication'
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_runtime_events AS runtime_event
          WHERE runtime_event.session_id = history.session_id
            AND runtime_event.turn_id = history.turn_id
            AND (
                NOT json_valid(runtime_event.msg_json)
                OR json_extract(runtime_event.msg_json, '$.kind') <> 'inter_agent_communication_received'
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM sessions AS session
          WHERE session.id = history.session_id
            AND session.active_turn_id = history.turn_id
      )
), ranked_candidates AS (
    SELECT
        id,
        session_id,
        old_turn_id,
        append_position,
        ROW_NUMBER() OVER (
            PARTITION BY session_id
            ORDER BY append_position ASC, id ASC
        ) - 1 AS new_sequence_no
    FROM session_candidates
)
INSERT INTO v42_session_history_items (
    id,
    session_id,
    old_turn_id,
    append_position,
    new_sequence_no
)
SELECT id, session_id, old_turn_id, append_position, new_sequence_no
FROM ranked_candidates;

CREATE TEMP TABLE v42_retired_pseudo_turns (
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    PRIMARY KEY (session_id, turn_id)
);

INSERT INTO v42_retired_pseudo_turns (session_id, turn_id)
SELECT history.session_id, history.turn_id
FROM protocol_history_items AS history
GROUP BY history.session_id, history.turn_id
HAVING COUNT(*) = (
    SELECT COUNT(*)
    FROM v42_session_history_items AS converted
    WHERE converted.session_id = history.session_id
      AND converted.old_turn_id = history.turn_id
)
AND COUNT(*) > 0;

DELETE FROM protocol_item_append_order
WHERE (source_kind = 'runtime_event' AND source_id IN (
           SELECT runtime_event.id
           FROM protocol_runtime_events AS runtime_event
           INNER JOIN v42_retired_pseudo_turns AS retired
             ON retired.session_id = runtime_event.session_id
            AND retired.turn_id = runtime_event.turn_id
       ))
   OR (source_kind = 'turn_item' AND source_id IN (
           SELECT turn_item.id
           FROM protocol_turn_items AS turn_item
           INNER JOIN v42_retired_pseudo_turns AS retired
             ON retired.session_id = turn_item.session_id
            AND retired.turn_id = turn_item.turn_id
       ));

DELETE FROM protocol_runtime_events
WHERE EXISTS (
    SELECT 1
    FROM v42_retired_pseudo_turns AS retired
    WHERE retired.session_id = protocol_runtime_events.session_id
      AND retired.turn_id = protocol_runtime_events.turn_id
);

DELETE FROM protocol_turn_items
WHERE EXISTS (
    SELECT 1
    FROM v42_retired_pseudo_turns AS retired
    WHERE retired.session_id = protocol_turn_items.session_id
      AND retired.turn_id = protocol_turn_items.turn_id
);

DROP INDEX IF EXISTS idx_protocol_history_collaboration_mode_session;
DROP INDEX IF EXISTS idx_protocol_history_items_session_turn_sequence;

CREATE TABLE protocol_history_items_v42 (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('turn', 'session')),
    turn_id TEXT,
    sequence_no INTEGER NOT NULL CHECK (sequence_no >= 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    payload_sha256 TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    CHECK (
        (scope_kind = 'turn' AND turn_id IS NOT NULL)
        OR (scope_kind = 'session' AND turn_id IS NULL)
    ),
    CHECK (json_type(payload_json, '$.kind') = 'text'),
    CHECK (
        (scope_kind = 'session' AND json_extract(payload_json, '$.kind') IN (
            'collaboration_mode_instruction',
            'inter_agent_communication'
        ))
        OR (scope_kind = 'turn' AND json_extract(payload_json, '$.kind') <> 'collaboration_mode_instruction')
    )
);

INSERT INTO protocol_history_items_v42 (
    id,
    session_id,
    scope_kind,
    turn_id,
    sequence_no,
    payload_json,
    payload_sha256,
    created_at_ms
)
SELECT
    history.id,
    history.session_id,
    CASE WHEN converted.id IS NULL THEN 'turn' ELSE 'session' END,
    CASE WHEN converted.id IS NULL THEN history.turn_id ELSE NULL END,
    COALESCE(converted.new_sequence_no, history.sequence_no),
    history.payload_json,
    history.payload_sha256,
    history.created_at_ms
FROM protocol_history_items AS history
LEFT JOIN v42_session_history_items AS converted ON converted.id = history.id;

DROP TABLE protocol_history_items;
ALTER TABLE protocol_history_items_v42 RENAME TO protocol_history_items;

CREATE UNIQUE INDEX idx_protocol_history_turn_sequence
    ON protocol_history_items(session_id, turn_id, sequence_no)
    WHERE scope_kind = 'turn';

CREATE UNIQUE INDEX idx_protocol_history_session_sequence
    ON protocol_history_items(session_id, sequence_no)
    WHERE scope_kind = 'session';

CREATE INDEX idx_protocol_history_collaboration_mode_session
    ON protocol_history_items(session_id, id)
    WHERE scope_kind = 'session'
      AND json_extract(payload_json, '$.kind') = 'collaboration_mode_instruction';

CREATE TABLE protocol_item_append_order_v42 (
    append_position INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('turn', 'session')),
    turn_id TEXT,
    sequence_no INTEGER NOT NULL CHECK (sequence_no >= 0),
    source_kind TEXT NOT NULL CHECK (
        source_kind IN ('runtime_event', 'history_item', 'turn_item')
    ),
    source_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_kind, source_id),
    CHECK (
        (scope_kind = 'turn' AND turn_id IS NOT NULL)
        OR (scope_kind = 'session' AND turn_id IS NULL AND source_kind = 'history_item')
    )
);

INSERT INTO protocol_item_append_order_v42 (
    append_position,
    session_id,
    scope_kind,
    turn_id,
    sequence_no,
    source_kind,
    source_id,
    created_at_ms
)
SELECT
    append_order.append_position,
    append_order.session_id,
    history.scope_kind,
    history.turn_id,
    history.sequence_no,
    append_order.source_kind,
    append_order.source_id,
    append_order.created_at_ms
FROM protocol_item_append_order AS append_order
INNER JOIN protocol_history_items AS history
  ON append_order.source_kind = 'history_item'
 AND history.id = append_order.source_id
 AND history.session_id = append_order.session_id

UNION ALL

SELECT
    append_order.append_position,
    append_order.session_id,
    'turn',
    runtime_event.turn_id,
    runtime_event.sequence_no,
    append_order.source_kind,
    append_order.source_id,
    append_order.created_at_ms
FROM protocol_item_append_order AS append_order
INNER JOIN protocol_runtime_events AS runtime_event
  ON append_order.source_kind = 'runtime_event'
 AND runtime_event.id = append_order.source_id
 AND runtime_event.session_id = append_order.session_id

UNION ALL

SELECT
    append_order.append_position,
    append_order.session_id,
    'turn',
    turn_item.turn_id,
    turn_item.sequence_no,
    append_order.source_kind,
    append_order.source_id,
    append_order.created_at_ms
FROM protocol_item_append_order AS append_order
INNER JOIN protocol_turn_items AS turn_item
  ON append_order.source_kind = 'turn_item'
 AND turn_item.id = append_order.source_id
 AND turn_item.session_id = append_order.session_id

ORDER BY append_position ASC;

DROP TABLE protocol_item_append_order;
ALTER TABLE protocol_item_append_order_v42 RENAME TO protocol_item_append_order;

CREATE INDEX idx_protocol_item_append_order_session_position
    ON protocol_item_append_order(session_id, append_position ASC);

CREATE INDEX idx_protocol_item_append_order_turn_position
    ON protocol_item_append_order(session_id, turn_id, append_position ASC)
    WHERE scope_kind = 'turn';

DELETE FROM protocol_turn_sequence_allocators
WHERE NOT EXISTS (
          SELECT 1
          FROM protocol_runtime_events AS runtime_event
          WHERE runtime_event.session_id = protocol_turn_sequence_allocators.session_id
            AND runtime_event.turn_id = protocol_turn_sequence_allocators.turn_id
      )
  AND NOT EXISTS (
          SELECT 1
          FROM protocol_history_items AS history
          WHERE history.session_id = protocol_turn_sequence_allocators.session_id
            AND history.scope_kind = 'turn'
            AND history.turn_id = protocol_turn_sequence_allocators.turn_id
      )
  AND NOT EXISTS (
          SELECT 1
          FROM protocol_turn_items AS turn_item
          WHERE turn_item.session_id = protocol_turn_sequence_allocators.session_id
            AND turn_item.turn_id = protocol_turn_sequence_allocators.turn_id
      );

DROP TABLE v42_retired_pseudo_turns;
DROP TABLE v42_session_history_items;

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (42, 'typed_history_scope');
