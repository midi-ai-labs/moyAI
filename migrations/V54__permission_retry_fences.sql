-- A failed AutoReview decision fences the same typed elevated-effect family until the
-- root session receives a new canonical UserTurn or delivered SteerTurn.  The lifecycle
-- also serializes equivalent reviews and the short allow-to-effect admission boundary.

CREATE TABLE permission_retry_fences (
    root_session_id TEXT NOT NULL
        REFERENCES sessions(id) ON DELETE CASCADE,
    family_version INTEGER NOT NULL CHECK(
        family_version > 0 AND family_version <= 4294967295
    ),
    family_sha256 TEXT NOT NULL CHECK(
        length(family_sha256) = 64
        AND family_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    authority_history_item_id TEXT NOT NULL
        REFERENCES protocol_history_items(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(
        state IN ('reviewing', 'allowed_pending', 'admitted', 'denied')
    ),
    review_id TEXT NOT NULL CHECK(length(review_id) > 0),
    identity_sha256 TEXT NOT NULL CHECK(
        length(identity_sha256) = 64
        AND identity_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    outcome TEXT CHECK(
        outcome IS NULL
        OR outcome IN (
            'guardian_denied',
            'invalid_decision',
            'deadline_exceeded',
            'guardian_error'
        )
    ),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(
        updated_at_ms >= 0 AND updated_at_ms >= created_at_ms
    ),
    PRIMARY KEY(root_session_id, family_version, family_sha256),
    CHECK(
        (state = 'denied' AND outcome IS NOT NULL)
        OR (state <> 'denied' AND outcome IS NULL)
    )
);

CREATE INDEX idx_permission_retry_fences_authority
    ON permission_retry_fences(root_session_id, authority_history_item_id);

CREATE TRIGGER validate_permission_retry_fence_before_insert
BEFORE INSERT ON permission_retry_fences
WHEN NEW.state <> 'reviewing'
  OR NEW.outcome IS NOT NULL
  OR NOT EXISTS (
      SELECT 1
      FROM protocol_history_items AS history
      INNER JOIN protocol_item_append_order AS append_order
        ON append_order.session_id = history.session_id
       AND append_order.source_kind = 'history_item'
       AND append_order.source_id = history.id
      WHERE history.id = NEW.authority_history_item_id
        AND history.session_id = NEW.root_session_id
        AND json_extract(history.payload_json, '$.kind')
            IN ('user_turn', 'steer_turn')
  )
BEGIN
    SELECT RAISE(
        ABORT,
        'permission retry fence must start as reviewing at an exact canonical root authority item'
    );
END;

CREATE TRIGGER validate_permission_retry_fence_before_update
BEFORE UPDATE ON permission_retry_fences
WHEN NOT (
    OLD.root_session_id = NEW.root_session_id
    AND OLD.family_version = NEW.family_version
    AND OLD.family_sha256 = NEW.family_sha256
    AND OLD.authority_history_item_id = NEW.authority_history_item_id
    AND OLD.review_id = NEW.review_id
    AND OLD.identity_sha256 = NEW.identity_sha256
    AND OLD.created_at_ms = NEW.created_at_ms
    AND NEW.updated_at_ms >= OLD.updated_at_ms
    AND EXISTS (
        SELECT 1
        FROM protocol_history_items AS history
        INNER JOIN protocol_item_append_order AS append_order
          ON append_order.session_id = history.session_id
         AND append_order.source_kind = 'history_item'
         AND append_order.source_id = history.id
        WHERE history.id = NEW.authority_history_item_id
          AND history.session_id = NEW.root_session_id
          AND json_extract(history.payload_json, '$.kind')
              IN ('user_turn', 'steer_turn')
    )
    AND (
        (
            OLD.state = 'reviewing'
            AND NEW.state = 'allowed_pending'
            AND NEW.outcome IS NULL
        )
        OR (
            OLD.state = 'reviewing'
            AND NEW.state = 'denied'
            AND NEW.outcome IS NOT NULL
        )
        OR (
            OLD.state = 'allowed_pending'
            AND NEW.state = 'admitted'
            AND NEW.outcome IS NULL
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid permission retry fence lifecycle transition');
END;

INSERT INTO moyai_schema_migrations(version, name)
VALUES (54, 'permission_retry_fences');
