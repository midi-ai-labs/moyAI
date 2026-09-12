Start a finite, managed foreground command, such as a development server.

Use the same shell syntax, environment and permission rules as `shell`. Run the
server in the foreground: do not detach it with Start-Process, nohup or `&`.
Ordinary `shell` stops descendants when its parent command exits.

This tool returns after process startup or startup failure. Keep the returned
process_id for `shell_status` and `shell_stop`; do not substitute an OS PID.
The command can outlive the agent turn, but stops on its timeout, starting-turn
cancellation, receiver revocation or application shutdown. It is not an installed
service and is not restored after restart. The default lifetime is the configured
shell maximum (normally 600000 ms); report that limit when handing over a server.

Running means the process started, not that a port or HTTP endpoint is ready.
Check readiness separately. Output is bounded and becomes available after exit.
Poll or stop to obtain the observed exit code; never infer a successful exit.
