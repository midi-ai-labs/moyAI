## Configured MCP connections

{{connections}}

Use `mcp_call` with a configured server ID and no `tool_name` to inspect its tools
and input schemas. Call supported tools with that server ID, the returned tool
name, and arguments matching its schema. Use known schemas without repeating
discovery. A configured connection does not prove the server is currently reachable.

General external MCP tools remain available. Connections marked `remote_agent`
refer to the legacy moyAI workflow: new `delegate_task` calls are retired; saved
job status, stop, and result operations remain available. For new work on another
project PC, use the advertised Hub project delegation tools. Local file and shell
tools still execute in the current environment. Connection loss alone is neither
completion nor a confirmed stop.
