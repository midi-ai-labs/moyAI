-- Terminal JSON is canonicalized by the Rust migration before these rows are
-- removed. Keep this file transaction-free: the caller owns one BEGIN IMMEDIATE
-- boundary around JSON rewrites, retired-row deletion, and the marker.

DELETE FROM protocol_item_append_order
WHERE source_kind = 'turn_item'
  AND source_id IN (
      SELECT turn_item.id
      FROM protocol_turn_items AS turn_item
      WHERE json_valid(turn_item.payload_json)
        AND json_extract(turn_item.payload_json, '$.kind') = 'warning'
        AND (
            turn_item.source_item_id IN (
                SELECT history_item.id
                FROM protocol_history_items AS history_item
                WHERE json_valid(history_item.payload_json)
                  AND json_extract(history_item.payload_json, '$.kind') = 'retry_decision'
            )
            OR EXISTS (
                SELECT 1
                FROM protocol_runtime_events AS runtime_event
                WHERE runtime_event.session_id = turn_item.session_id
                  AND runtime_event.turn_id = turn_item.turn_id
                  AND runtime_event.sequence_no = turn_item.sequence_no
                  AND json_valid(runtime_event.msg_json)
                  AND json_extract(runtime_event.msg_json, '$.kind') = 'retry_scheduled'
            )
        )
  );

DELETE FROM protocol_turn_items
WHERE json_valid(payload_json)
  AND json_extract(payload_json, '$.kind') = 'warning'
  AND (
      source_item_id IN (
          SELECT id
          FROM protocol_history_items
          WHERE json_valid(payload_json)
            AND json_extract(payload_json, '$.kind') = 'retry_decision'
      )
      OR EXISTS (
          SELECT 1
          FROM protocol_runtime_events AS runtime_event
          WHERE runtime_event.session_id = protocol_turn_items.session_id
            AND runtime_event.turn_id = protocol_turn_items.turn_id
            AND runtime_event.sequence_no = protocol_turn_items.sequence_no
            AND json_valid(runtime_event.msg_json)
            AND json_extract(runtime_event.msg_json, '$.kind') = 'retry_scheduled'
      )
  );

DELETE FROM protocol_item_append_order
WHERE source_kind = 'history_item'
  AND source_id IN (
      SELECT id
      FROM protocol_history_items
      WHERE json_valid(payload_json)
        AND json_extract(payload_json, '$.kind') = 'retry_decision'
  );

DELETE FROM protocol_history_items
WHERE json_valid(payload_json)
  AND json_extract(payload_json, '$.kind') = 'retry_decision';

DELETE FROM protocol_item_append_order
WHERE source_kind = 'runtime_event'
  AND source_id IN (
      SELECT id
      FROM protocol_runtime_events
      WHERE json_valid(msg_json)
        AND json_extract(msg_json, '$.kind') IN (
            'thread_configured',
            'assistant_text_delta',
            'reasoning_summary_delta',
            'retry_scheduled',
            'history_item_recorded'
        )
  );

DELETE FROM protocol_runtime_events
WHERE json_valid(msg_json)
  AND json_extract(msg_json, '$.kind') IN (
      'thread_configured',
      'assistant_text_delta',
      'reasoning_summary_delta',
      'retry_scheduled',
      'history_item_recorded'
  );

INSERT OR IGNORE INTO moyai_schema_migrations (version, name)
VALUES (39, 'terminal_outcome_cutover');
