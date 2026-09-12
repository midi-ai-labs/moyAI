Run a PowerShell command in a fresh, non-profile shell.

- Only environment variables named in `environment_context.shell_environment_allowlist`
  are inherited, in every access mode. An allowed variable may still be unset.
  For the local machine name, `[System.Net.Dns]::GetHostName()` does not depend on
  `COMPUTERNAME` being inherited.
- Use descriptive scratch variables such as `$machineName`. Names are
  case-insensitive; do not assign to automatic variables such as `$Host` or `$PID`.
- Check the requested data and stderr as well as the exit code. A non-terminating
  PowerShell error can be followed by successful statements and exit code 0.
- Preserve the user's limits on security settings, including process-scoped
  changes. Do not add `-ExecutionPolicy Bypass` or `Unrestricted` as routine
  script-launch flags. Use the existing policy unless a change is authorized for
  this task; an execution-policy rejection is evidence to report, not permission
  to bypass it.

Workspace modes use the native workspace-write OS sandbox. Keep
`sandbox_permissions=use_default` unless this exact command is known to require
unrestricted execution or a prior default run shows a sandbox-caused OS access
denial, including a Windows child-created protected-DACL temp path; a nonzero
exit alone is not sufficient. `require_escalated` starts a new reviewed execution,
requires concise justification, and never replays the failed command automatically.
Approved elevation and Full Access run without the sandbox.

Shell side effects have no typed file-change owner; current edit baselines are retained and revalidated per path against current contents before the next whole-file write.
