# Review the current work

Before finishing, inspect the actual work and its security consequences. Include changed files, commands, external actions, and affected callers. A successful command or an earlier summary is not evidence that the current result is safe.

Check the areas that apply:

- Access: who can invoke the behavior, read the data, or change the target? Check ownership and authorization at the point of use, including other users and workspaces.
- Untrusted input: follow external values into paths, commands, queries, rendered output, and agent instructions. Check validation, escaping, and boundaries between data and instructions.
- Sensitive data: examine storage, logs, errors, generated documents, and outbound requests for exposed credentials or private information. Do not print secrets as review evidence.
- Network and deployment: examine exposed endpoints, authentication, transport, destination selection, and unsafe defaults relevant to this change.
- Dependencies and execution: inspect new dependencies, permissions, and executable inputs. Use existing authorized tools; do not install scanners, fetch packages, or probe external systems without authorization.
- Failures: examine error paths, cancellation, cleanup, partial changes, and misleading success reports. Run relevant existing checks when authorized and practical.

Read the current material after the latest change. Distinguish observed findings, hypotheses, completed checks, and unexamined areas. If a check fails, report the failure. Do not invent tool results or claim that a review proves the absence of vulnerabilities.

Before the final response, call `security_review` using its advertised schema. Record scope, findings, limitations, and the IDs of checks actually completed. Explain why an area does not apply based on the task and its effects. If further work changes the reviewed result, inspect that result and record a new review.

If a check's ID is absent from the visible text, retrieve it before recording the review:

1. Call `read_history` with `query` set to distinctive text from the check command or its tool name, such as `shell`. Search is case-sensitive and matches string values; `tool_call` can find call records. Follow `next_after` until the relevant record is found or the search ends.
2. Select the match for the exact operation you ran. Pass its `matches[].item_id` to `read_history` as `item_id`. Read the returned `text` as JSON; follow `next` if incomplete. Copy that record's `call_id` unchanged into `check_tool_call_ids`. An `item_id` is not a `call_id`.
3. Use the current turn's operation, not an older run of the same command. A call record alone is not a successful result. Do not invent an ID or infer success. If the operation cannot be identified, leave it unverified and explain the missing evidence in `limitations`.

Further references, when useful:
- https://owasp.org/www-project-application-security-verification-standard/
- https://cheatsheetseries.owasp.org/
