-- Persist one atomic provider protocol profile instead of independently mutable
-- catalog and generation modes. Root sessions also gain a nullable connection
-- snapshot. The service creation boundary requires a snapshot for every new
-- root; NULL remains readable only for pre-V60 sessions and low-level migration
-- fixtures whose exact historical profile and credential references cannot be
-- recovered from durable state.

ALTER TABLE sessions
ADD COLUMN provider_connection_json TEXT NULL CHECK (
    provider_connection_json IS NULL
    OR COALESCE(
        json_valid(provider_connection_json) = 1
        AND json_type(provider_connection_json, '$') = 'object'
        AND json_type(provider_connection_json, '$.profile') = 'text'
        AND json_extract(provider_connection_json, '$.profile') IN (
            'lm_studio',
            'openai_compatible',
            'openai_responses',
            'lm_studio_chat_completions'
        )
        AND (
            json_type(provider_connection_json, '$.api_key_env') = 'null'
            OR (
                json_type(provider_connection_json, '$.api_key_env') = 'text'
                AND length(trim(json_extract(
                    provider_connection_json,
                    '$.api_key_env'
                ))) > 0
                AND json_extract(provider_connection_json, '$.api_key_env')
                    = trim(json_extract(provider_connection_json, '$.api_key_env'))
                AND json_extract(provider_connection_json, '$.api_key_env')
                    NOT GLOB '*[^A-Za-z0-9_]*'
            )
        )
        AND json_type(provider_connection_json, '$.extra_headers') = 'object',
        0
    )
);

-- SQLite CHECK expressions cannot traverse each member of a JSON object. These
-- triggers keep non-text custom-header values out at the write boundary; the
-- typed reopen audit additionally validates names, values, and exact fields.
CREATE TRIGGER validate_session_provider_connection_headers_before_insert
BEFORE INSERT ON sessions
WHEN NEW.provider_connection_json IS NOT NULL
 AND json_valid(NEW.provider_connection_json) = 1
 AND json_type(NEW.provider_connection_json, '$.extra_headers') = 'object'
 AND EXISTS (
     SELECT 1
     FROM json_each(NEW.provider_connection_json, '$.extra_headers')
     WHERE type <> 'text'
 )
BEGIN
    SELECT RAISE(
        ABORT,
        'provider connection custom-header values must be JSON strings'
    );
END;

CREATE TRIGGER validate_session_provider_connection_headers_before_update
BEFORE UPDATE OF provider_connection_json ON sessions
WHEN NEW.provider_connection_json IS NOT NULL
 AND json_valid(NEW.provider_connection_json) = 1
 AND json_type(NEW.provider_connection_json, '$.extra_headers') = 'object'
 AND EXISTS (
     SELECT 1
     FROM json_each(NEW.provider_connection_json, '$.extra_headers')
     WHERE type <> 'text'
 )
BEGIN
    SELECT RAISE(
        ABORT,
        'provider connection custom-header values must be JSON strings'
    );
END;

-- V55 side chats contain the exact legacy pair, so their hidden canonical
-- conversations can be upgraded eagerly. Ordinary root/child rows stay NULL.
UPDATE sessions
SET provider_connection_json = json_object(
        'profile', (
            SELECT CASE
                WHEN binding.provider_metadata_mode = 'lm_studio_native_required'
                 AND binding.provider_api_mode = 'responses'
                    THEN 'lm_studio'
                WHEN binding.provider_metadata_mode = 'openai_compatible_only'
                 AND binding.provider_api_mode = 'chat_completions'
                    THEN 'openai_compatible'
                WHEN binding.provider_metadata_mode = 'openai_compatible_only'
                 AND binding.provider_api_mode = 'responses'
                    THEN 'openai_responses'
                WHEN binding.provider_metadata_mode = 'lm_studio_native_required'
                 AND binding.provider_api_mode = 'chat_completions'
                    THEN 'lm_studio_chat_completions'
            END
            FROM side_chat_bindings AS binding
            WHERE binding.conversation_session_id = sessions.id
        ),
        'api_key_env', NULL,
        'extra_headers', json('{}')
    )
WHERE EXISTS (
    SELECT 1
    FROM side_chat_bindings AS binding
    WHERE binding.conversation_session_id = sessions.id
);

DROP TRIGGER validate_side_chat_binding_before_insert;
DROP TRIGGER validate_side_chat_binding_before_update;
DROP TRIGGER prevent_tombstoned_side_chat_binding_update;
DROP TRIGGER prevent_active_side_chat_binding_delete;
DROP INDEX idx_side_chat_bindings_conversation;

ALTER TABLE side_chat_bindings RENAME TO side_chat_bindings_v55;

CREATE TABLE side_chat_bindings (
    id TEXT PRIMARY KEY CHECK(length(id) = 26),
    owner_session_id TEXT NOT NULL UNIQUE
        REFERENCES sessions(id) ON DELETE CASCADE,
    conversation_session_id TEXT NOT NULL UNIQUE
        REFERENCES sessions(id) ON DELETE CASCADE,
    base_url TEXT NOT NULL CHECK(length(trim(base_url)) > 0),
    model TEXT NOT NULL CHECK(length(trim(model)) > 0),
    provider_profile TEXT NOT NULL CHECK(
        provider_profile IN (
            'lm_studio',
            'openai_compatible',
            'openai_responses',
            'lm_studio_chat_completions'
        )
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

INSERT INTO side_chat_bindings (
    id, owner_session_id, conversation_session_id,
    base_url, model, provider_profile,
    context_window, max_output_tokens,
    request_timeout_ms, connect_timeout_ms, max_retries,
    supports_images, supports_tools, supports_reasoning,
    persisted_draft, draft_revision, request_generation,
    delete_requested_at_ms, context_scope,
    created_at_ms, updated_at_ms
)
SELECT
    id, owner_session_id, conversation_session_id,
    base_url, model,
    CASE
        WHEN provider_metadata_mode = 'lm_studio_native_required'
         AND provider_api_mode = 'responses'
            THEN 'lm_studio'
        WHEN provider_metadata_mode = 'openai_compatible_only'
         AND provider_api_mode = 'chat_completions'
            THEN 'openai_compatible'
        WHEN provider_metadata_mode = 'openai_compatible_only'
         AND provider_api_mode = 'responses'
            THEN 'openai_responses'
        WHEN provider_metadata_mode = 'lm_studio_native_required'
         AND provider_api_mode = 'chat_completions'
            THEN 'lm_studio_chat_completions'
    END,
    context_window, max_output_tokens,
    request_timeout_ms, connect_timeout_ms, max_retries,
    supports_images, supports_tools, supports_reasoning,
    persisted_draft, draft_revision, request_generation,
    delete_requested_at_ms, context_scope,
    created_at_ms, updated_at_ms
FROM side_chat_bindings_v55;

DROP TABLE side_chat_bindings_v55;

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
      AND json_extract(conversation.provider_connection_json, '$.profile')
            = NEW.provider_profile
      AND json_type(conversation.provider_connection_json, '$.api_key_env') = 'null'
      AND json_type(conversation.provider_connection_json, '$.extra_headers') = 'object'
      AND NOT EXISTS (
          SELECT 1
          FROM json_each(conversation.provider_connection_json, '$.extra_headers')
      )
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
            AND OLD.provider_profile = NEW.provider_profile
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
          AND json_extract(conversation.provider_connection_json, '$.profile')
                = NEW.provider_profile
          AND json_type(conversation.provider_connection_json, '$.api_key_env') = 'null'
          AND json_type(conversation.provider_connection_json, '$.extra_headers') = 'object'
          AND NOT EXISTS (
              SELECT 1
              FROM json_each(conversation.provider_connection_json, '$.extra_headers')
          )
    )
    AND (
        (
            OLD.base_url = NEW.base_url
            AND OLD.model = NEW.model
            AND OLD.provider_profile = NEW.provider_profile
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
            AND OLD.provider_profile = NEW.provider_profile
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
VALUES (60, 'provider_connection_profiles');
