ALTER TABLE side_chat_bindings
ADD COLUMN provider_route_kind TEXT NOT NULL DEFAULT 'direct'
CHECK(provider_route_kind IN ('direct', 'hub'));

CREATE TRIGGER validate_side_chat_route_kind_before_update
BEFORE UPDATE OF provider_route_kind ON side_chat_bindings
WHEN OLD.provider_route_kind <> NEW.provider_route_kind AND NOT (
    OLD.provider_route_kind = 'hub' AND NEW.provider_route_kind = 'direct'
    AND OLD.request_generation = NEW.request_generation
    AND OLD.draft_revision = NEW.draft_revision
    AND OLD.persisted_draft = NEW.persisted_draft
    AND OLD.delete_requested_at_ms IS NULL AND NEW.delete_requested_at_ms IS NULL
    AND OLD.system_prompt = NEW.system_prompt
    AND OLD.context_window = NEW.context_window
    AND OLD.request_timeout_ms = NEW.request_timeout_ms
    AND OLD.connect_timeout_ms = NEW.connect_timeout_ms
    AND OLD.max_retries = NEW.max_retries
    AND EXISTS (SELECT 1 FROM sessions WHERE id = NEW.conversation_session_id AND status <> 'running')
)
BEGIN SELECT RAISE(ABORT, 'side chat direct provider can only be captured once while idle'); END;

INSERT INTO moyai_schema_migrations(version, name) VALUES(66, 'side_chat_route_kind');
