Delegate one child job to an allowed PC and resume after the child returns its
terminal result. Stage only needed files with `shared_upload_file`, then pass the
returned asset IDs in `input_refs`; the child receives immutable copies.

Use the project context and the target environment's declared role when choosing
the PC. In `prompt`, state the overall objective, the current stage when useful,
the bounded work for this child, its constraints, completion checks, and what to
return. Keep global design, placement, and integration with the parent; report
missing facts without inventing them. Project descriptions do not grant permissions.

Call this tool alone after ordinary managed commands finish. For a server used
across PCs, distinguish who hosts it:

- This job hosts; child tests: start the server here with
  `shell_start(retain_for_delegation=true)`, then delegate the test.
- Child hosts; this job tests after the child returns: ask the child to use
  `shell_start(retain_after_turn=true)` and report the endpoint. Stop temporary test servers after verification; keep a preview only when requested.

A child's successful response does not prove a server is still running. Its
`retained_service` result and `shared_services` report retained processes;
HTTP readiness is a separate check. Stop a temporary server after verification
using `shared_stop_service` for a retained service on another PC.

The prompt is shared with the selected PC; include only information this project
permits sharing. Inspect a failed or cancelled child result before continuing.
Use `shared_job_artifacts` to inspect files published by the child.
