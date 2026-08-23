-- A side chat is a hidden canonical conversation bound to one visible owning session.
-- Canonical history, turn admission, terminal state, and crash recovery remain owned by
-- the referenced conversation session; this table owns only the binding, provider role,
-- and unsent draft.

CREATE TABLE side_chat_bindings (
    id TEXT PRIMARY KEY CHECK(length(id) = 26),
    owner_session_id TEXT NOT NULL UNIQUE
        REFERENCES sessions(id) ON DELETE CASCADE,
    conversation_session_id TEXT NOT NULL UNIQUE
        REFERENCES sessions(id) ON DELETE CASCADE,
    base_url TEXT NOT NULL CHECK(length(trim(base_url)) > 0),
    model TEXT NOT NULL CHECK(length(trim(model)) > 0),
    provider_metadata_mode TEXT NOT NULL CHECK(
        provider_metadata_mode IN (
            'lm_studio_native_required',
            'openai_compatible_only'
        )
    ),
    provider_api_mode TEXT NOT NULL CHECK(
        provider_api_mode IN ('chat_completions', 'responses')
    ),
    context_window INTEGER NOT NULL CHECK(
        context_window > 0 AND context_window <= 4294967295
    ),
    max_output_tokens INTEGER NOT NULL CHECK(
        max_output_tokens > 0 AND max_output_tokens <= 4294967295
    ),
    request_timeout_ms INTEGER NOT NULL CHECK(request_timeout_ms > 0),
    connect_timeout_ms INTEGER NOT NULL CHECK(connect_timeout_ms > 0),
    max_retries INTEGER NOT NULL CHECK(max_retries >= 0 AND max_retries <= 255),
    supports_images INTEGER NOT NULL CHECK(supports_images IN (0, 1)),
    supports_tools INTEGER NOT NULL CHECK(supports_tools IN (0, 1)),
    supports_reasoning INTEGER NOT NULL CHECK(supports_reasoning IN (0, 1)),
    persisted_draft TEXT NOT NULL CHECK(
        length(CAST(persisted_draft AS BLOB)) <= 1048576
    ),
    draft_revision INTEGER NOT NULL CHECK(draft_revision >= 0),
    request_generation INTEGER NOT NULL CHECK(request_generation >= 0),
    delete_requested_at_ms INTEGER NULL CHECK(
        delete_requested_at_ms IS NULL OR delete_requested_at_ms >= 0
    ),
    context_scope TEXT NOT NULL CHECK(context_scope = 'general'),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(
        updated_at_ms >= 0 AND updated_at_ms >= created_at_ms
    ),
    CHECK(owner_session_id <> conversation_session_id)
);

CREATE INDEX idx_side_chat_bindings_conversation
    ON side_chat_bindings(conversation_session_id, owner_session_id);

CREATE TRIGGER validate_side_chat_binding_before_insert
BEFORE INSERT ON side_chat_bindings
WHEN NOT EXISTS (
    SELECT 1
    FROM sessions AS owner
    INNER JOIN sessions AS conversation
      ON conversation.id = NEW.conversation_session_id
    WHERE owner.id = NEW.owner_session_id
      AND owner.project_id = conversation.project_id
      AND conversation.status = 'idle'
      AND conversation.active_run_id IS NULL
      AND conversation.active_turn_id IS NULL
      AND conversation.active_run_lease_expires_at_ms IS NULL
      AND conversation.model_name = NEW.model
      AND conversation.base_url = NEW.base_url
      AND NEW.delete_requested_at_ms IS NULL
      AND NOT EXISTS (
          SELECT 1
          FROM side_chat_bindings AS existing
          WHERE existing.conversation_session_id = NEW.owner_session_id
      )
      AND NOT EXISTS (
          SELECT 1
          FROM session_spawn_edges AS edge
          WHERE edge.root_session_id = NEW.conversation_session_id
             OR edge.parent_session_id = NEW.conversation_session_id
             OR edge.child_session_id = NEW.conversation_session_id
      )
)
BEGIN
    SELECT RAISE(
        ABORT,
        'side chat binding must own one fresh hidden canonical conversation in the same project'
    );
END;

CREATE TRIGGER validate_side_chat_binding_before_update
BEFORE UPDATE ON side_chat_bindings
WHEN NOT (
    OLD.id = NEW.id
    AND OLD.owner_session_id = NEW.owner_session_id
    AND OLD.conversation_session_id = NEW.conversation_session_id
    AND OLD.context_scope = NEW.context_scope
    AND OLD.created_at_ms = NEW.created_at_ms
    AND NEW.updated_at_ms >= OLD.updated_at_ms
    AND NEW.draft_revision IN (OLD.draft_revision, OLD.draft_revision + 1)
    AND NEW.request_generation IN (
        OLD.request_generation,
        OLD.request_generation + 1
    )
    AND (
        NEW.delete_requested_at_ms IS OLD.delete_requested_at_ms
        OR (
            OLD.delete_requested_at_ms IS NULL
            AND NEW.delete_requested_at_ms IS NOT NULL
            AND NEW.delete_requested_at_ms >= 0
            AND OLD.base_url = NEW.base_url
            AND OLD.model = NEW.model
            AND OLD.provider_metadata_mode = NEW.provider_metadata_mode
            AND OLD.provider_api_mode = NEW.provider_api_mode
            AND OLD.context_window = NEW.context_window
            AND OLD.max_output_tokens = NEW.max_output_tokens
            AND OLD.request_timeout_ms = NEW.request_timeout_ms
            AND OLD.connect_timeout_ms = NEW.connect_timeout_ms
            AND OLD.max_retries = NEW.max_retries
            AND OLD.supports_images = NEW.supports_images
            AND OLD.supports_tools = NEW.supports_tools
            AND OLD.supports_reasoning = NEW.supports_reasoning
            AND OLD.persisted_draft = NEW.persisted_draft
            AND OLD.draft_revision = NEW.draft_revision
            AND OLD.request_generation = NEW.request_generation
        )
    )
    AND (
        (NEW.draft_revision = OLD.draft_revision
            AND NEW.persisted_draft = OLD.persisted_draft)
        OR NEW.draft_revision = OLD.draft_revision + 1
    )
    AND EXISTS (
        SELECT 1
        FROM sessions AS conversation
        WHERE conversation.id = NEW.conversation_session_id
          AND conversation.model_name = NEW.model
          AND conversation.base_url = NEW.base_url
    )
    AND (
        (
            OLD.base_url = NEW.base_url
            AND OLD.model = NEW.model
            AND OLD.provider_metadata_mode = NEW.provider_metadata_mode
            AND OLD.provider_api_mode = NEW.provider_api_mode
            AND OLD.context_window = NEW.context_window
            AND OLD.max_output_tokens = NEW.max_output_tokens
            AND OLD.request_timeout_ms = NEW.request_timeout_ms
            AND OLD.connect_timeout_ms = NEW.connect_timeout_ms
            AND OLD.max_retries = NEW.max_retries
            AND OLD.supports_images = NEW.supports_images
            AND OLD.supports_tools = NEW.supports_tools
            AND OLD.supports_reasoning = NEW.supports_reasoning
        )
        OR EXISTS (
            SELECT 1
            FROM sessions AS conversation
            WHERE conversation.id = NEW.conversation_session_id
              AND conversation.status <> 'running'
        )
    )
    AND (
        NEW.request_generation = OLD.request_generation
        OR (
            (
                (
                    NEW.draft_revision = OLD.draft_revision
                    AND NEW.persisted_draft = OLD.persisted_draft
                )
                OR (
                    NEW.draft_revision = OLD.draft_revision + 1
                    AND NEW.persisted_draft = ''
                )
            )
            AND OLD.base_url = NEW.base_url
            AND OLD.model = NEW.model
            AND OLD.provider_metadata_mode = NEW.provider_metadata_mode
            AND OLD.provider_api_mode = NEW.provider_api_mode
            AND OLD.context_window = NEW.context_window
            AND OLD.max_output_tokens = NEW.max_output_tokens
            AND OLD.request_timeout_ms = NEW.request_timeout_ms
            AND OLD.connect_timeout_ms = NEW.connect_timeout_ms
            AND OLD.max_retries = NEW.max_retries
            AND OLD.supports_images = NEW.supports_images
            AND OLD.supports_tools = NEW.supports_tools
            AND OLD.supports_reasoning = NEW.supports_reasoning
            AND EXISTS (
                SELECT 1
                FROM sessions AS conversation
                WHERE conversation.id = NEW.conversation_session_id
                  AND conversation.status = 'running'
                  AND conversation.active_run_id IS NOT NULL
                  AND conversation.active_turn_id IS NOT NULL
                  AND conversation.active_run_lease_expires_at_ms IS NOT NULL
                  AND EXISTS (
                      SELECT 1
                      FROM protocol_history_items AS history
                      INNER JOIN protocol_item_append_order AS append_order
                        ON append_order.session_id = history.session_id
                       AND append_order.source_kind = 'history_item'
                       AND append_order.source_id = history.id
                      WHERE history.session_id = conversation.id
                        AND history.turn_id = conversation.active_turn_id
                        AND json_extract(history.payload_json, '$.kind') = 'user_turn'
                  )
            )
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid side chat binding transition');
END;

CREATE TRIGGER prevent_tombstoned_side_chat_binding_update
BEFORE UPDATE ON side_chat_bindings
WHEN OLD.delete_requested_at_ms IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'side chat deletion is already requested');
END;

CREATE TRIGGER prevent_active_side_chat_binding_delete
BEFORE DELETE ON side_chat_bindings
WHEN EXISTS (
    SELECT 1
    FROM sessions AS conversation
    WHERE conversation.id = OLD.conversation_session_id
      AND conversation.status = 'running'
)
BEGIN
    SELECT RAISE(ABORT, 'active side chat must settle before deletion');
END;

INSERT INTO moyai_schema_migrations(version, name)
VALUES (55, 'durable_side_chats');
