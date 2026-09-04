ALTER TABLE side_chat_bindings
ADD COLUMN system_prompt TEXT NOT NULL DEFAULT '' CHECK(
    typeof(system_prompt) = 'text'
    AND system_prompt = trim(
        system_prompt,
        char(
            9, 10, 11, 12, 13, 32, 133, 160, 5760,
            8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
            8232, 8233, 8239, 8287, 12288
        )
    )
    AND length(system_prompt) <= 16384
);

DROP TRIGGER validate_side_chat_binding_before_update;

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
            AND OLD.system_prompt = NEW.system_prompt
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
            AND OLD.system_prompt = NEW.system_prompt
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
            AND OLD.system_prompt = NEW.system_prompt
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

INSERT INTO moyai_schema_migrations(version, name)
VALUES (61, 'side_chat_system_prompt');
