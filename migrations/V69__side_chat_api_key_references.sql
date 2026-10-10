-- Side conversations may explicitly own an API-key environment-variable reference.
-- The existing session CHECK and typed provider-connection audit validate its name.
-- Keep the released ownership/transition rules and empty-header boundary intact.

DROP TRIGGER validate_side_chat_binding_before_insert;
DROP TRIGGER validate_side_chat_binding_before_update;

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
VALUES (69, 'side_chat_api_key_references');
