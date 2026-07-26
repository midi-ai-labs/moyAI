-- Inter-agent communication is queued before it becomes model-visible.  The
-- mailbox row owns the immutable payload and its lifecycle; protocol history
-- is only the delivered projection.

CREATE TABLE agent_mailbox_messages (
    id TEXT PRIMARY KEY,
    root_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    author_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    recipient_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    payload_sha256 TEXT NOT NULL,
    trigger_turn INTEGER NOT NULL CHECK(trigger_turn IN (0, 1)),
    state TEXT NOT NULL CHECK(state IN ('pending', 'delivered', 'discarded')),
    delivered_turn_id TEXT,
    delivered_history_item_id TEXT UNIQUE
        REFERENCES protocol_history_items(id) ON DELETE RESTRICT,
    resolved_by_terminal_event_id TEXT
        REFERENCES protocol_runtime_events(id) ON DELETE RESTRICT,
    discarded_by_stopped_session_id TEXT,
    discarded_after_append_position INTEGER,
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= created_at_ms),
    resolved_at_ms INTEGER,
    FOREIGN KEY(
        discarded_by_stopped_session_id,
        discarded_after_append_position
    ) REFERENCES agent_tree_stop_fences(
        stopped_session_id,
        after_append_position
    ) ON DELETE RESTRICT,
    CHECK(json_extract(payload_json, '$.kind') = 'inter_agent_communication'),
    CHECK(json_type(payload_json, '$.communication.author') = 'text'),
    CHECK(json_type(payload_json, '$.communication.recipient') = 'text'),
    CHECK(json_type(payload_json, '$.communication.content') = 'text'),
    CHECK(
        json_extract(payload_json, '$.communication.trigger_turn')
        = trigger_turn
    ),
    CHECK(
        (state = 'pending'
         AND delivered_turn_id IS NULL
         AND delivered_history_item_id IS NULL
         AND resolved_by_terminal_event_id IS NULL
         AND discarded_by_stopped_session_id IS NULL
         AND discarded_after_append_position IS NULL
         AND resolved_at_ms IS NULL)
        OR
        (state = 'delivered'
         AND delivered_history_item_id = id
         AND resolved_by_terminal_event_id IS NULL
         AND discarded_by_stopped_session_id IS NULL
         AND discarded_after_append_position IS NULL
         AND resolved_at_ms IS NOT NULL)
        OR
        (state = 'discarded'
         AND delivered_turn_id IS NULL
         AND delivered_history_item_id IS NULL
         AND resolved_at_ms IS NOT NULL
         AND (
             (resolved_by_terminal_event_id IS NOT NULL
              AND discarded_by_stopped_session_id IS NULL
              AND discarded_after_append_position IS NULL)
             OR
             (resolved_by_terminal_event_id IS NULL
              AND discarded_by_stopped_session_id IS NOT NULL
              AND discarded_after_append_position IS NOT NULL)
         ))
    ),
    CHECK(resolved_at_ms IS NULL OR resolved_at_ms >= created_at_ms)
);

CREATE INDEX idx_agent_mailbox_recipient_state_order
    ON agent_mailbox_messages(
        recipient_session_id,
        state,
        created_at_ms,
        id
    );

CREATE INDEX idx_agent_mailbox_root_state
    ON agent_mailbox_messages(root_session_id, state, id);

-- Resolve the durable identity of every released IAC before changing any
-- history or append-order row.  NOT NULL/CHECK failures intentionally abort an
-- ambiguous upgrade instead of inventing an agent identity.
CREATE TEMP TABLE v50_iac_backfill (
    id TEXT PRIMARY KEY,
    root_session_id TEXT NOT NULL,
    author_session_id TEXT NOT NULL,
    recipient_session_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    trigger_turn INTEGER NOT NULL CHECK(trigger_turn IN (0, 1)),
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('turn', 'session')),
    turn_id TEXT,
    append_position INTEGER NOT NULL,
    sequence_no INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expected_recipient_path TEXT NOT NULL,
    CHECK(
        json_extract(payload_json, '$.communication.recipient')
        = expected_recipient_path
    )
);

WITH source AS (
    SELECT
        history.id,
        COALESCE(recipient_edge.root_session_id, history.session_id)
            AS root_session_id,
        history.session_id AS recipient_session_id,
        history.payload_json,
        history.payload_sha256,
        CAST(
            json_extract(
                history.payload_json,
                '$.communication.trigger_turn'
            ) AS INTEGER
        ) AS trigger_turn,
        history.scope_kind,
        history.turn_id,
        append_order.append_position,
        history.sequence_no,
        history.created_at_ms,
        CASE
            WHEN recipient_edge.child_session_id IS NULL THEN '/root'
            ELSE recipient_edge.agent_path
        END AS expected_recipient_path,
        json_extract(
            history.payload_json,
            '$.communication.author'
        ) AS author_path
    FROM protocol_history_items AS history
    INNER JOIN protocol_item_append_order AS append_order
      ON append_order.session_id = history.session_id
     AND append_order.source_kind = 'history_item'
     AND append_order.source_id = history.id
    LEFT JOIN session_spawn_edges AS recipient_edge
      ON recipient_edge.child_session_id = history.session_id
    WHERE json_extract(history.payload_json, '$.kind')
          = 'inter_agent_communication'
)
INSERT INTO v50_iac_backfill (
    id,
    root_session_id,
    author_session_id,
    recipient_session_id,
    payload_json,
    payload_sha256,
    trigger_turn,
    scope_kind,
    turn_id,
    append_position,
    sequence_no,
    created_at_ms,
    expected_recipient_path
)
SELECT
    source.id,
    source.root_session_id,
    CASE
        WHEN source.author_path = '/root' THEN source.root_session_id
        ELSE (
            SELECT author_edge.child_session_id
            FROM session_spawn_edges AS author_edge
            WHERE author_edge.root_session_id = source.root_session_id
              AND author_edge.agent_path = source.author_path
        )
    END,
    source.recipient_session_id,
    source.payload_json,
    source.payload_sha256,
    source.trigger_turn,
    source.scope_kind,
    source.turn_id,
    source.append_position,
    source.sequence_no,
    source.created_at_ms,
    source.expected_recipient_path
FROM source
WHERE json_extract(
          source.payload_json,
          '$.communication.recipient'
      ) = source.expected_recipient_path
  AND (
      source.author_path = '/root'
      OR EXISTS (
          SELECT 1
          FROM session_spawn_edges AS author_edge
          WHERE author_edge.root_session_id = source.root_session_id
            AND author_edge.agent_path = source.author_path
      )
  );

CREATE TEMP TABLE v50_fenced_mail (
    id TEXT PRIMARY KEY,
    stopped_session_id TEXT NOT NULL,
    after_append_position INTEGER NOT NULL
);

WITH RECURSIVE fenced_scope(
    stopped_session_id,
    after_append_position,
    session_id
) AS (
    SELECT
        fence.stopped_session_id,
        fence.after_append_position,
        fence.stopped_session_id
    FROM agent_tree_stop_fences AS fence
    UNION ALL
    SELECT
        fenced.stopped_session_id,
        fenced.after_append_position,
        child.child_session_id
    FROM fenced_scope AS fenced
    INNER JOIN session_spawn_edges AS child
      ON child.parent_session_id = fenced.session_id
), ranked AS (
    SELECT
        mail.id,
        fenced.stopped_session_id,
        fenced.after_append_position,
        ROW_NUMBER() OVER (
            PARTITION BY mail.id
            ORDER BY
                fenced.after_append_position ASC,
                fenced.stopped_session_id ASC
        ) AS rank
    FROM v50_iac_backfill AS mail
    INNER JOIN fenced_scope AS fenced
      ON fenced.session_id = mail.recipient_session_id
     AND mail.append_position <= fenced.after_append_position
    WHERE mail.scope_kind = 'session'
      AND NOT EXISTS (
          SELECT 1
          FROM protocol_history_items AS compaction,
               json_each(
                   compaction.payload_json,
                   '$.replacement_item_ids'
               ) AS replaced
          WHERE json_extract(compaction.payload_json, '$.kind') = 'compaction'
            AND replaced.value = mail.id
      )
)
INSERT INTO v50_fenced_mail (
    id,
    stopped_session_id,
    after_append_position
)
SELECT id, stopped_session_id, after_append_position
FROM ranked
WHERE rank = 1;

CREATE TEMP TABLE v50_mailbox_resolution (
    id TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK(state IN ('pending', 'delivered', 'discarded')),
    delivered_turn_id TEXT,
    discarded_by_stopped_session_id TEXT,
    discarded_after_append_position INTEGER
);

INSERT INTO v50_mailbox_resolution (
    id,
    state,
    delivered_turn_id,
    discarded_by_stopped_session_id,
    discarded_after_append_position
)
SELECT
    mail.id,
    CASE
        WHEN mail.scope_kind = 'turn' THEN 'delivered'
        WHEN fenced.id IS NOT NULL THEN 'discarded'
        WHEN EXISTS (
            SELECT 1
            FROM protocol_history_items AS compaction,
                 json_each(
                     compaction.payload_json,
                     '$.replacement_item_ids'
                 ) AS replaced
            WHERE json_extract(compaction.payload_json, '$.kind') = 'compaction'
              AND replaced.value = mail.id
        ) THEN 'delivered'
        WHEN EXISTS (
            SELECT 1
            FROM protocol_item_append_order AS later
            WHERE later.session_id = mail.recipient_session_id
              AND later.scope_kind = 'turn'
              AND later.append_position > mail.append_position
        ) THEN 'delivered'
        ELSE 'pending'
    END,
    CASE
        WHEN mail.scope_kind = 'turn' THEN mail.turn_id
        ELSE (
            SELECT later.turn_id
            FROM protocol_item_append_order AS later
            WHERE later.session_id = mail.recipient_session_id
              AND later.scope_kind = 'turn'
              AND later.append_position > mail.append_position
            ORDER BY later.append_position ASC
            LIMIT 1
        )
    END,
    fenced.stopped_session_id,
    fenced.after_append_position
FROM v50_iac_backfill AS mail
LEFT JOIN v50_fenced_mail AS fenced ON fenced.id = mail.id;

INSERT INTO agent_mailbox_messages (
    id,
    root_session_id,
    author_session_id,
    recipient_session_id,
    payload_json,
    payload_sha256,
    trigger_turn,
    state,
    delivered_turn_id,
    delivered_history_item_id,
    resolved_by_terminal_event_id,
    discarded_by_stopped_session_id,
    discarded_after_append_position,
    created_at_ms,
    updated_at_ms,
    resolved_at_ms
)
SELECT
    mail.id,
    mail.root_session_id,
    mail.author_session_id,
    mail.recipient_session_id,
    mail.payload_json,
    mail.payload_sha256,
    mail.trigger_turn,
    resolution.state,
    resolution.delivered_turn_id,
    CASE WHEN resolution.state = 'delivered' THEN mail.id END,
    NULL,
    CASE
        WHEN resolution.state = 'discarded'
        THEN resolution.discarded_by_stopped_session_id
    END,
    CASE
        WHEN resolution.state = 'discarded'
        THEN resolution.discarded_after_append_position
    END,
    mail.created_at_ms,
    mail.created_at_ms,
    CASE
        WHEN resolution.state = 'pending' THEN NULL
        ELSE mail.created_at_ms
    END
FROM v50_iac_backfill AS mail
INNER JOIN v50_mailbox_resolution AS resolution ON resolution.id = mail.id;

-- Preserve the global enqueue boundary.  Pending/discarded legacy session
-- history becomes a mailbox marker at the exact same append position.
DROP INDEX idx_protocol_item_append_order_session_position;
DROP INDEX idx_protocol_item_append_order_turn_position;

PRAGMA legacy_alter_table = ON;
ALTER TABLE protocol_item_append_order
    RENAME TO protocol_item_append_order_v49;

CREATE TABLE protocol_item_append_order (
    append_position INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('turn', 'session')),
    turn_id TEXT,
    sequence_no INTEGER NOT NULL CHECK(sequence_no >= 0),
    source_kind TEXT NOT NULL CHECK(
        source_kind IN (
            'runtime_event',
            'history_item',
            'turn_item',
            'mailbox_message'
        )
    ),
    source_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_kind, source_id),
    CHECK(
        (scope_kind = 'turn'
         AND turn_id IS NOT NULL
         AND source_kind <> 'mailbox_message')
        OR
        (scope_kind = 'session'
         AND turn_id IS NULL
         AND source_kind IN ('history_item', 'mailbox_message'))
    )
);

INSERT INTO protocol_item_append_order (
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
    append_order.scope_kind,
    append_order.turn_id,
    append_order.sequence_no,
    CASE
        WHEN resolution.state IN ('pending', 'discarded')
        THEN 'mailbox_message'
        ELSE append_order.source_kind
    END,
    append_order.source_id,
    append_order.created_at_ms
FROM protocol_item_append_order_v49 AS append_order
LEFT JOIN v50_mailbox_resolution AS resolution
  ON append_order.source_kind = 'history_item'
 AND resolution.id = append_order.source_id
ORDER BY append_order.append_position ASC;

DROP TABLE protocol_item_append_order_v49;

CREATE INDEX idx_protocol_item_append_order_session_position
    ON protocol_item_append_order(session_id, append_position ASC);

CREATE INDEX idx_protocol_item_append_order_turn_position
    ON protocol_item_append_order(session_id, turn_id, append_position ASC)
    WHERE scope_kind = 'turn';

-- Completion receipts and owner-resume state now reference the durable mailbox
-- identity.  The released physical column names remain stable; their FK owner
-- and semantics change from history projection to mailbox source.
ALTER TABLE agent_completion_handoffs
    RENAME TO agent_completion_handoffs_v49;

CREATE TABLE agent_completion_handoffs (
    child_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    child_turn_id TEXT NOT NULL,
    child_terminal_event_id TEXT NOT NULL UNIQUE
        REFERENCES protocol_runtime_events(id) ON DELETE RESTRICT,
    parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    parent_history_item_id TEXT NOT NULL UNIQUE
        REFERENCES agent_mailbox_messages(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(child_session_id, child_turn_id),
    CHECK(child_session_id <> parent_session_id)
);

INSERT INTO agent_completion_handoffs
SELECT * FROM agent_completion_handoffs_v49;

DROP TABLE agent_completion_handoffs_v49;

ALTER TABLE agent_owner_resume_requests
    RENAME TO agent_owner_resume_requests_v49;

CREATE TABLE agent_owner_resume_requests (
    owner_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    source_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    source_history_item_id TEXT NOT NULL
        REFERENCES agent_mailbox_messages(id) ON DELETE CASCADE,
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

INSERT INTO agent_owner_resume_requests
SELECT * FROM agent_owner_resume_requests_v49;

DROP TABLE agent_owner_resume_requests_v49;
PRAGMA legacy_alter_table = OFF;

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

DELETE FROM protocol_history_items
WHERE id IN (
    SELECT resolution.id
    FROM v50_mailbox_resolution AS resolution
    WHERE resolution.state IN ('pending', 'discarded')
);

CREATE TRIGGER validate_agent_mailbox_message_before_insert
BEFORE INSERT ON agent_mailbox_messages
WHEN
    NEW.state <> 'pending'
    OR NEW.delivered_turn_id IS NOT NULL
    OR NEW.delivered_history_item_id IS NOT NULL
    OR NEW.resolved_by_terminal_event_id IS NOT NULL
    OR NEW.discarded_by_stopped_session_id IS NOT NULL
    OR NEW.discarded_after_append_position IS NOT NULL
    OR NEW.resolved_at_ms IS NOT NULL
    OR NOT EXISTS (
        SELECT 1
        FROM sessions AS root
        INNER JOIN sessions AS author
          ON author.id = NEW.author_session_id
         AND author.project_id = root.project_id
        INNER JOIN sessions AS recipient
          ON recipient.id = NEW.recipient_session_id
         AND recipient.project_id = root.project_id
        LEFT JOIN session_spawn_edges AS author_edge
          ON author_edge.root_session_id = NEW.root_session_id
         AND author_edge.child_session_id = NEW.author_session_id
        LEFT JOIN session_spawn_edges AS recipient_edge
          ON recipient_edge.root_session_id = NEW.root_session_id
         AND recipient_edge.child_session_id = NEW.recipient_session_id
        WHERE root.id = NEW.root_session_id
          AND NOT EXISTS (
              SELECT 1
              FROM session_spawn_edges AS root_edge
              WHERE root_edge.child_session_id = NEW.root_session_id
          )
          AND (
              (NEW.author_session_id = NEW.root_session_id
               AND json_extract(
                       NEW.payload_json,
                       '$.communication.author'
                   ) = '/root')
              OR
              (author_edge.child_session_id IS NOT NULL
               AND json_extract(
                       NEW.payload_json,
                       '$.communication.author'
                   ) = author_edge.agent_path)
          )
          AND (
              (NEW.recipient_session_id = NEW.root_session_id
               AND json_extract(
                       NEW.payload_json,
                       '$.communication.recipient'
                   ) = '/root')
              OR
              (recipient_edge.child_session_id IS NOT NULL
               AND json_extract(
                       NEW.payload_json,
                       '$.communication.recipient'
                   ) = recipient_edge.agent_path)
          )
    )
BEGIN
    SELECT RAISE(ABORT, 'invalid pending agent mailbox message');
END;

CREATE TRIGGER validate_agent_mailbox_append_order_before_insert
BEFORE INSERT ON protocol_item_append_order
WHEN NEW.source_kind = 'mailbox_message'
 AND NOT EXISTS (
     SELECT 1
     FROM agent_mailbox_messages AS mailbox
     WHERE mailbox.id = NEW.source_id
       AND mailbox.recipient_session_id = NEW.session_id
       AND mailbox.state = 'pending'
       AND NEW.scope_kind = 'session'
       AND NEW.turn_id IS NULL
 )
BEGIN
    SELECT RAISE(ABORT, 'mailbox append order must reference one pending recipient message');
END;

CREATE TRIGGER validate_agent_mailbox_message_before_update
BEFORE UPDATE ON agent_mailbox_messages
WHEN NOT (
    OLD.state IN ('delivered', 'discarded')
    AND NEW.state = OLD.state
    AND OLD.author_session_id IS NOT NULL
    AND NEW.author_session_id IS NULL
    AND NEW.id = OLD.id
    AND NEW.root_session_id = OLD.root_session_id
    AND NEW.recipient_session_id = OLD.recipient_session_id
    AND NEW.payload_json = OLD.payload_json
    AND NEW.payload_sha256 = OLD.payload_sha256
    AND NEW.trigger_turn = OLD.trigger_turn
    AND NEW.delivered_turn_id IS OLD.delivered_turn_id
    AND NEW.delivered_history_item_id IS OLD.delivered_history_item_id
    AND NEW.resolved_by_terminal_event_id
        IS OLD.resolved_by_terminal_event_id
    AND NEW.discarded_by_stopped_session_id
        IS OLD.discarded_by_stopped_session_id
    AND NEW.discarded_after_append_position
        IS OLD.discarded_after_append_position
    AND NEW.created_at_ms = OLD.created_at_ms
    AND NEW.updated_at_ms = OLD.updated_at_ms
    AND NEW.resolved_at_ms IS OLD.resolved_at_ms
)
AND (
    NEW.id <> OLD.id
    OR NEW.root_session_id <> OLD.root_session_id
    OR NEW.author_session_id IS NOT OLD.author_session_id
    OR NEW.recipient_session_id <> OLD.recipient_session_id
    OR NEW.payload_json <> OLD.payload_json
    OR NEW.payload_sha256 <> OLD.payload_sha256
    OR NEW.trigger_turn <> OLD.trigger_turn
    OR NEW.created_at_ms <> OLD.created_at_ms
    OR OLD.state <> 'pending'
    OR NEW.state NOT IN ('delivered', 'discarded')
    OR (
        NEW.state = 'delivered'
        AND NOT EXISTS (
            SELECT 1
            FROM protocol_history_items AS history
            INNER JOIN protocol_item_append_order AS history_order
              ON history_order.session_id = history.session_id
             AND history_order.source_kind = 'history_item'
             AND history_order.source_id = history.id
            INNER JOIN protocol_item_append_order AS mailbox_order
              ON mailbox_order.session_id = OLD.recipient_session_id
             AND mailbox_order.source_kind = 'mailbox_message'
             AND mailbox_order.source_id = OLD.id
            WHERE history.id = OLD.id
              AND history.session_id = OLD.recipient_session_id
              AND history.scope_kind = 'turn'
              AND history.turn_id = NEW.delivered_turn_id
              AND history.payload_json = OLD.payload_json
              AND history.payload_sha256 = OLD.payload_sha256
              AND history_order.append_position >
                  mailbox_order.append_position
        )
    )
    OR (
        NEW.state = 'discarded'
        AND NEW.discarded_by_stopped_session_id IS NOT NULL
        AND NOT EXISTS (
            WITH RECURSIVE stopped_scope(session_id) AS (
                SELECT NEW.discarded_by_stopped_session_id
                UNION ALL
                SELECT child.child_session_id
                FROM session_spawn_edges AS child
                INNER JOIN stopped_scope AS parent
                  ON child.parent_session_id = parent.session_id
            )
            SELECT 1
            FROM agent_tree_stop_fences AS fence
            INNER JOIN protocol_item_append_order AS mailbox_order
              ON mailbox_order.session_id = OLD.recipient_session_id
             AND mailbox_order.source_kind = 'mailbox_message'
             AND mailbox_order.source_id = OLD.id
            WHERE fence.stopped_session_id =
                  NEW.discarded_by_stopped_session_id
              AND fence.after_append_position =
                  NEW.discarded_after_append_position
              AND mailbox_order.append_position <=
                  fence.after_append_position
              AND EXISTS (
                  SELECT 1
                  FROM stopped_scope
                  WHERE session_id = OLD.recipient_session_id
              )
        )
    )
    OR (
        NEW.state = 'discarded'
        AND NEW.resolved_by_terminal_event_id IS NOT NULL
        AND NOT EXISTS (
            SELECT 1
            FROM protocol_runtime_events AS terminal
            INNER JOIN protocol_item_append_order AS terminal_order
              ON terminal_order.session_id = terminal.session_id
             AND terminal_order.source_kind = 'runtime_event'
             AND terminal_order.source_id = terminal.id
            INNER JOIN protocol_item_append_order AS mailbox_order
              ON mailbox_order.session_id = OLD.recipient_session_id
             AND mailbox_order.source_kind = 'mailbox_message'
             AND mailbox_order.source_id = OLD.id
            WHERE terminal.id = NEW.resolved_by_terminal_event_id
              AND terminal.session_id = OLD.recipient_session_id
              AND json_extract(terminal.msg_json, '$.kind')
                  = 'turn_terminal'
              AND terminal_order.append_position >
                  mailbox_order.append_position
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid agent mailbox lifecycle transition');
END;

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
    INNER JOIN protocol_item_append_order AS terminal_order
      ON terminal_order.session_id = terminal.session_id
     AND terminal_order.source_kind = 'runtime_event'
     AND terminal_order.source_id = terminal.id
    INNER JOIN agent_mailbox_messages AS mailbox
      ON mailbox.id = NEW.parent_history_item_id
     AND mailbox.recipient_session_id = NEW.parent_session_id
     AND mailbox.author_session_id = NEW.child_session_id
     AND mailbox.state = 'pending'
     AND mailbox.trigger_turn = 0
    INNER JOIN protocol_item_append_order AS mailbox_order
      ON mailbox_order.session_id = mailbox.recipient_session_id
     AND mailbox_order.source_kind = 'mailbox_message'
     AND mailbox_order.source_id = mailbox.id
     AND mailbox_order.append_position > terminal_order.append_position
    WHERE edge.child_session_id = NEW.child_session_id
      AND edge.parent_session_id = NEW.parent_session_id
      AND instr(
              json_extract(mailbox.payload_json, '$.communication.content'),
              'Message Type: FINAL_ANSWER' || char(10)
               || 'Task name: ' ||
               CASE
                   WHEN edge.parent_session_id = edge.root_session_id THEN '/root'
                   ELSE (
                       SELECT parent_edge.agent_path
                       FROM session_spawn_edges AS parent_edge
                       WHERE parent_edge.child_session_id =
                             edge.parent_session_id
                   )
               END
               || char(10) || 'Sender: ' || edge.agent_path
               || char(10) || 'Payload:' || char(10)
          ) = 1
)
BEGIN
    SELECT RAISE(
        ABORT,
        'agent completion handoff must link one child terminal to its immediate parent pending FINAL mailbox message'
    );
END;

CREATE TRIGGER prevent_agent_completion_handoff_update
BEFORE UPDATE ON agent_completion_handoffs
BEGIN
    SELECT RAISE(ABORT, 'agent completion handoffs are immutable');
END;

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
        INNER JOIN agent_mailbox_messages AS mailbox
          ON mailbox.id = handoff.parent_history_item_id
         AND mailbox.id = NEW.source_history_item_id
         AND mailbox.recipient_session_id = NEW.owner_session_id
         AND mailbox.state = 'pending'
         AND mailbox.trigger_turn = 0
        WHERE handoff.parent_session_id = NEW.owner_session_id
    )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'owner resume request must link a non-root owner to a direct-child pending FINAL mailbox message'
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

DROP TABLE v50_mailbox_resolution;
DROP TABLE v50_fenced_mail;
DROP TABLE v50_iac_backfill;

INSERT INTO moyai_schema_migrations(version, name)
VALUES (50, 'durable_agent_mailbox');
