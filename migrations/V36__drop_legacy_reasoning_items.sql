BEGIN IMMEDIATE;

DELETE FROM protocol_item_append_order
WHERE (source_kind = 'runtime_event' AND source_id IN (
           SELECT id
           FROM protocol_runtime_events
           WHERE CASE
               WHEN json_valid(msg_json)
               THEN json_extract(msg_json, '$.kind') = 'reasoning_delta'
               ELSE 0
           END
       ))
   OR (source_kind = 'history_item' AND source_id IN (
           SELECT id
           FROM protocol_history_items
           WHERE CASE
               WHEN json_valid(payload_json)
               THEN json_extract(payload_json, '$.kind') IN ('reasoning', 'prompt_dispatch')
               ELSE 0
           END
       ))
   OR (source_kind = 'turn_item' AND source_id IN (
           SELECT id
           FROM protocol_turn_items
           WHERE CASE
               WHEN json_valid(payload_json)
               THEN json_extract(payload_json, '$.kind') IN ('reasoning', 'prompt_dispatch')
               ELSE 0
           END
       ));

DELETE FROM protocol_runtime_events
WHERE CASE
    WHEN json_valid(msg_json)
    THEN json_extract(msg_json, '$.kind') = 'reasoning_delta'
    ELSE 0
END;

DELETE FROM protocol_history_items
WHERE CASE
    WHEN json_valid(payload_json)
    THEN json_extract(payload_json, '$.kind') IN ('reasoning', 'prompt_dispatch')
    ELSE 0
END;

DELETE FROM protocol_turn_items
WHERE CASE
    WHEN json_valid(payload_json)
    THEN json_extract(payload_json, '$.kind') IN ('reasoning', 'prompt_dispatch')
    ELSE 0
END;

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (36, 'drop_legacy_reasoning_items');

COMMIT;
