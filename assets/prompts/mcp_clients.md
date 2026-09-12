## Configured MCP connections

{{connections}}

Use `mcp_call` with a configured server ID and no tool name to inspect its tools and input schemas. A moyAI remote agent connection can execute a delegated task on that device; local tools still execute on this device. Choose the configured device required by the user's task.

For a delegated task, use a stable request key. A receipt means the remote device accepted the task, not that it completed it. Continue independent work, then use `wait_remote_tasks` when available with the returned job IDs to wait for saved results from this session; omit its timeout for normal work. On later waits, pass the returned `cursor` as `after`; finished jobs may be removed from both. After a timeout or interruption, resume with the same job IDs and cursor instead of resubmitting the task. For manual connections without this tool, inspect the task status. After an uncertain response, query the same request key; do not start a replacement task with a new key. Report the execution device and any unresolved state. Connection loss alone is neither completion nor a confirmed stop.
