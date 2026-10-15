Start a managed foreground command, such as a development server.

Use the same shell syntax, environment and permission rules as `shell`. Run the
server in the foreground: do not detach it with Start-Process, nohup or `&`.
Ordinary `shell` stops descendants when its parent command exits.

This tool returns after process startup or startup failure. Keep the returned
process_id for `shell_status` and `shell_stop`; do not substitute an OS PID.
For a shared job, select the lifetime for the PC that starts the command:

- `retain_for_delegation=true`: this job hosts the server while it calls
  `shared_delegate` to have another PC do work. With this option alone, the server
  stops when this job finishes.
- `retain_after_turn=true`: this job hosts the server and returns so its caller
  can use or test it. Use this for a server requested on this PC that another PC
  will verify after receiving the result, or for a requested preview.
- Without either option, a shared job must finish its commands before delegating,
  and its commands stop when the job finishes. Local commands can outlive a turn.

Only one managed command may still be running when a shared job delegates or
finishes; finish other commands before handing over the server. Starting-turn
cancellation, receiver revocation, control-connection loss and application shutdown
still stop it. It is not an installed service and is not restored after restart.
Normally omit `timeout_ms`: the process has no elapsed-time limit and stays owned
until it exits or is stopped. The LLM response timeout does not limit its lifetime.
Supply a positive timeout only when the task explicitly needs a finite command.
Stop temporary services after their caller finishes verification; retain after the
request only when continued use was requested.

Running means the process started, not that a port or HTTP endpoint is ready.
Check readiness separately. Output is bounded and becomes available after exit.
Poll or stop to obtain the observed exit code; never infer a successful exit.
