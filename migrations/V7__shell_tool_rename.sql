UPDATE tool_calls
SET tool_name = 'shell'
WHERE tool_name = 'bash';

UPDATE session_state
SET failure_tool_name = 'shell'
WHERE failure_tool_name = 'bash';

UPDATE message_parts
SET payload_json = REPLACE(payload_json, '"tool_name":"bash"', '"tool_name":"shell"')
WHERE part_kind = 'tool_call'
  AND INSTR(payload_json, '"tool_name":"bash"') > 0;
