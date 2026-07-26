use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use encoding_rs::SHIFT_JIS;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

use crate::config::ShellFamily;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::os_sandbox::ProcessSandboxPlan;
use crate::tool::process::ManagedProcess;
#[cfg(test)]
use crate::tool::process::{ProcessTerminationStep, process_tree_termination_plan};
use crate::tool::registry::Tool;
use crate::tool::sandbox_process::{
    SandboxedProcessRequest, captured_process_environment, execute_workspace_write,
};
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, GuardedPath, PathGuard};

#[derive(Debug, Deserialize)]
pub struct ShellInput {
    pub command: String,
    pub workdir: Option<Utf8PathBuf>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub sandbox_permissions: ShellSandboxPermissions,
    #[serde(default)]
    pub justification: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellSandboxPermissions {
    #[default]
    UseDefault,
    RequireEscalated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShellSandboxFailureHint {
    WorkspaceWriteEffectTempAccessDenied,
}

#[derive(Debug, Default)]
pub struct ShellTool;

#[async_trait(?Send)]
impl Tool for ShellTool {
    fn spec(&self) -> ToolSpec {
        let description = if cfg!(windows) {
            "Run a PowerShell command. Workspace modes use the native workspace-write OS sandbox. Keep sandbox_permissions=use_default unless this exact command is known to require unrestricted execution or a prior default run shows a sandbox-caused OS access denial, including a Windows child-created protected-DACL temp path; a nonzero exit alone is not sufficient. require_escalated starts a new reviewed execution, requires concise justification, and never replays the failed command automatically. Approved elevation and Full Access run without the sandbox. Shell side effects have no typed file-change owner, so every edit baseline for the current session is invalidated before execution."
        } else {
            "Run a bash command. Workspace modes require a supported native workspace-write OS sandbox. Keep sandbox_permissions=use_default unless this exact command is known to require unrestricted execution or a prior default run shows a sandbox-caused OS access denial; a nonzero exit alone is not sufficient. require_escalated starts a new reviewed execution, requires concise justification, and never replays the failed command automatically. Approved elevation and Full Access run without the sandbox. Shell side effects have no typed file-change owner, so every edit baseline for the current session is invalidated before execution."
        };
        ToolSpec {
            name: ToolName::Shell,
            effect: crate::tool::ToolEffectPolicy::destructive(),
            description,
            input_schema: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": { "type": "string" },
                    "workdir": { "type": "string" },
                    "timeout_ms": { "type": "integer" },
                    "description": { "type": "string" },
                    "sandbox_permissions": {
                        "type": "string",
                        "enum": ["use_default", "require_escalated"],
                        "description": "Keep use_default unless this exact command is known to require unrestricted execution or a prior default run shows a sandbox-caused OS access denial. A nonzero exit alone is not sufficient. require_escalated starts a new reviewed execution, requires justification, and never automatically replays the failed command."
                    },
                    "justification": { "type": "string" }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<ShellInput>(raw_arguments)?;
        let ShellPermissionIntent {
            guarded,
            description,
            details,
            targets,
            outside_workspace,
            risks,
        } = shell_permission_intent(ctx.workspace, ctx.config, &input)?;
        let effect_admission = ctx
            .confirm_if_needed_with_details(
                AccessKind::Shell,
                description.clone(),
                details,
                targets,
                outside_workspace,
                risks,
            )
            .await?;
        let timeout_ms = input
            .timeout_ms
            .unwrap_or(ctx.config.shell.default_timeout_ms)
            .min(ctx.config.shell.max_timeout_ms);
        ctx.run_mutation_fence.assert_owned().await?;
        effect_admission.admit()?;
        PathGuard::revalidate(&guarded)?;
        let effect_may_start = !ctx.cancel.is_cancelled();
        if effect_may_start {
            ctx.services
                .edit_safety
                .invalidate_session(ctx.session.session.id)?;
        }
        let output = execute_shell_command(
            &ctx.config.shell,
            &guarded.absolute,
            &input.command,
            timeout_ms,
            ctx.config.tool_output.max_bytes.max(1),
            ctx.cancel.clone(),
            effect_admission.sandbox_plan(),
        )
        .await?;
        let merged_output = format_shell_output_for_display(
            &input.command,
            &output.stdout,
            &output.stderr,
            output.exit_code,
            output.timed_out,
            output.cancelled,
        );
        let preview = ctx.services.truncator.preview(
            merged_output,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;
        let change_evidence_status = if output.effect_started {
            "unknown"
        } else {
            "not_started"
        };
        let sandbox_failure_hint =
            classify_shell_sandbox_failure_hint(effect_admission.sandbox_plan(), &output);

        Ok(ToolResult {
            title: description,
            output_text: preview.preview_text,
            metadata: json!({
                "exit_code": output.exit_code,
                "timeout": output.timed_out,
                "cancelled": output.cancelled,
                "effect_started": output.effect_started,
                "stdout_capture_truncated": output.stdout_truncated,
                "stderr_capture_truncated": output.stderr_truncated,
                "truncated": preview.truncated,
                "success": output.exit_code == Some(0) && !output.timed_out && !output.cancelled && !output.cleanup_failed,
                "cleanup_failed": output.cleanup_failed,
                "sandbox_failure_hint": sandbox_failure_hint,
                "change_evidence": {
                    "status": change_evidence_status,
                    "effects_unknown": output.effect_started,
                    "session_edit_baselines_invalidated": effect_may_start,
                },
                "sandbox": effect_admission.sandbox_plan().audit_description(),
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: preview.internal_file_lease,
        })
    }
}

struct ShellPermissionIntent {
    guarded: GuardedPath,
    description: String,
    details: Vec<String>,
    targets: Vec<Utf8PathBuf>,
    outside_workspace: bool,
    risks: Vec<PermissionRisk>,
}

fn shell_permission_intent(
    workspace: &crate::workspace::Workspace,
    config: &crate::config::ResolvedConfig,
    input: &ShellInput,
) -> Result<ShellPermissionIntent, ToolError> {
    let requested_elevation = shell_requested_elevation(input)?;
    let requested_workdir = input
        .workdir
        .clone()
        .unwrap_or_else(|| Utf8PathBuf::from("."));
    let guarded = PathGuard::require_path(workspace, &requested_workdir, AccessKind::Shell)?;
    if !guarded.absolute.is_dir() {
        return Err(ToolError::Message(format!(
            "shell workdir `{}` is not a directory",
            guarded.absolute
        )));
    }
    let outside_workspace = requested_elevation
        || (!guarded.inside_workspace && !guarded.trusted_external)
        || references_outside_workspace_from(workspace, &guarded.absolute, &input.command);
    let description = if input.description.trim().is_empty() {
        default_description(&input.command)
    } else {
        input.description.clone()
    };
    let mut risks = shell_permission_risks_from(workspace, &guarded, &input.command);
    if command_mentions_configured_instruction_target(
        workspace,
        &config.instructions.additional_files,
        &guarded,
        &input.command,
    ) && !risks.contains(&PermissionRisk::ProtectedWorkspaceAuthority)
    {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    let mut details = shell_permission_details(&input.command, &guarded.absolute);
    let targets = shell_permission_targets(&guarded, &input.command);
    if requested_elevation {
        details.push(format!(
            "Requested sandbox elevation: {}",
            input.justification.trim()
        ));
    }
    Ok(ShellPermissionIntent {
        guarded,
        description,
        details,
        targets,
        outside_workspace,
        risks,
    })
}

fn shell_permission_targets(guarded_workdir: &GuardedPath, command: &str) -> Vec<Utf8PathBuf> {
    let mut targets = vec![guarded_workdir.absolute.clone()];
    for extracted in extract_absolute_paths(&guarded_workdir.absolute, command) {
        let target =
            crate::workspace::project::normalize_path(&guarded_workdir.absolute, &extracted)
                .unwrap_or(extracted);
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

fn shell_requested_elevation(input: &ShellInput) -> Result<bool, ToolError> {
    let requested = input.sandbox_permissions == ShellSandboxPermissions::RequireEscalated;
    if requested && input.justification.trim().is_empty() {
        return Err(ToolError::Message(
            "shell sandbox_permissions=require_escalated requires a concise justification"
                .to_string(),
        ));
    }
    Ok(requested)
}

fn format_shell_output_for_display(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
) -> String {
    [
        format!("Command: {}", command.trim()),
        format!(
            "Exit code: {}{}",
            exit_code
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            if timed_out {
                " (timeout)"
            } else if cancelled {
                " (cancelled)"
            } else {
                ""
            }
        ),
        format!(
            "Stdout:\n{}",
            if stdout.trim().is_empty() {
                "(empty)"
            } else {
                stdout.trim_end()
            }
        ),
        format!(
            "Stderr:\n{}",
            if stderr.trim().is_empty() {
                "(empty)"
            } else {
                stderr.trim_end()
            }
        ),
    ]
    .join("\n\n")
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    effect_started: bool,
    cleanup_failed: bool,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn classify_shell_sandbox_failure_hint(
    sandbox_plan: &ProcessSandboxPlan,
    output: &CommandOutput,
) -> Option<ShellSandboxFailureHint> {
    if !matches!(sandbox_plan, ProcessSandboxPlan::WorkspaceWrite(_))
        || !output.effect_started
        || !output.exit_code.is_some_and(|code| code != 0)
        || output.timed_out
        || output.cancelled
        || output.cleanup_failed
    {
        return None;
    }

    output
        .stdout
        .lines()
        .chain(output.stderr.lines())
        .any(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("permissionerror: [winerror 5]") && line.contains("moyai-sandbox-effect-")
        })
        .then_some(ShellSandboxFailureHint::WorkspaceWriteEffectTempAccessDenied)
}

enum ShellWaitOutcome {
    Exited(Result<std::process::ExitStatus, std::io::Error>),
    TimedOut,
    Cancelled,
}

async fn execute_shell_command(
    shell: &crate::config::ShellConfig,
    workdir: &Utf8Path,
    command_text: &str,
    timeout_ms: u64,
    max_output_bytes: usize,
    cancel: CancellationToken,
    sandbox_plan: &ProcessSandboxPlan,
) -> Result<CommandOutput, ToolError> {
    if cancel.is_cancelled() {
        return Ok(CommandOutput {
            stdout: String::new(),
            stderr: "command cancelled by user".to_string(),
            exit_code: None,
            timed_out: false,
            cancelled: true,
            effect_started: false,
            cleanup_failed: false,
            stdout_truncated: false,
            stderr_truncated: false,
        });
    }
    let family = shell.family.unwrap_or(if cfg!(windows) {
        ShellFamily::PowerShell
    } else {
        ShellFamily::Bash
    });
    let environment = captured_process_environment(shell);
    let programs = resolve_shell_programs(shell, family, &environment);
    execute_shell_command_with_programs(
        shell,
        workdir,
        command_text,
        timeout_ms,
        max_output_bytes,
        cancel,
        sandbox_plan,
        family,
        environment,
        programs,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_shell_command_with_programs(
    shell: &crate::config::ShellConfig,
    workdir: &Utf8Path,
    command_text: &str,
    timeout_ms: u64,
    max_output_bytes: usize,
    cancel: CancellationToken,
    sandbox_plan: &ProcessSandboxPlan,
    family: ShellFamily,
    environment: std::collections::HashMap<String, String>,
    programs: Vec<String>,
) -> Result<CommandOutput, ToolError> {
    let arguments = match family {
        ShellFamily::PowerShell => vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            command_text.to_string(),
        ],
        ShellFamily::Bash => vec!["-lc".to_string(), command_text.to_string()],
    };

    if let ProcessSandboxPlan::NoProcess = sandbox_plan {
        return Err(ToolError::SandboxExecution(
            crate::tool::sandbox_process::SandboxExecutionError::InvalidProfile(
                "shell process was not authorized by this tool admission".to_string(),
            ),
        ));
    }
    if let ProcessSandboxPlan::WorkspaceWrite(profile) = sandbox_plan {
        let mut completed = None;
        let program_count = programs.len();
        for (index, program) in programs.iter().enumerate() {
            let mut argv = Vec::with_capacity(arguments.len() + 1);
            argv.push(program.clone());
            argv.extend(arguments.iter().cloned());
            match execute_workspace_write(
                profile.clone(),
                SandboxedProcessRequest {
                    argv,
                    cwd: workdir.to_path_buf(),
                    environment: environment.clone(),
                    stdin: Vec::new(),
                    timeout_ms,
                    max_output_bytes,
                    hide_window: shell.hide_windows,
                    cancel: cancel.clone(),
                },
            )
            .await
            {
                Ok(output) => {
                    completed = Some(output);
                    break;
                }
                Err(crate::tool::sandbox_process::SandboxExecutionError::Spawn(_))
                    if index + 1 < program_count =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let completed = completed.ok_or_else(|| {
            ToolError::Message("no shell program candidate could be launched".to_string())
        })?;
        let stdout = captured_shell_text(&completed.stdout.bytes, completed.stdout.truncated);
        let mut stderr = captured_shell_text(&completed.stderr.bytes, completed.stderr.truncated);
        let cleanup_error = completed.cleanup_error();
        if let Some(cleanup_error) = &cleanup_error {
            if !stderr.is_empty() {
                stderr.push('\n');
            }
            stderr.push_str(&cleanup_error);
        }
        return Ok(CommandOutput {
            stdout,
            stderr,
            exit_code: completed.exit_code,
            timed_out: completed.timed_out,
            cancelled: completed.cancelled,
            effect_started: completed.effect_started,
            cleanup_failed: cleanup_error.is_some(),
            stdout_truncated: completed.stdout.truncated,
            stderr_truncated: completed.stderr.truncated,
        });
    }

    let mut process = None;
    let program_count = programs.len();
    for (index, program) in programs.iter().enumerate() {
        let mut command = Command::new(program);
        command.args(&arguments);
        command.current_dir(workdir.as_std_path());
        apply_captured_shell_environment(&mut command, &environment);
        match ManagedProcess::spawn(command, shell.hide_windows, max_output_bytes).await {
            Ok(spawned) => {
                process = Some(spawned);
                break;
            }
            Err(error)
                if index + 1 < program_count && retryable_windows_shell_spawn_error(&error) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut process = process
        .ok_or_else(|| ToolError::Message("no shell program candidate could be launched".into()))?;
    let wait_outcome = tokio::select! {
        _ = cancel.cancelled() => ShellWaitOutcome::Cancelled,
        result = timeout(Duration::from_millis(timeout_ms), process.wait()) => match result {
            Ok(result) => ShellWaitOutcome::Exited(result),
            Err(_) => ShellWaitOutcome::TimedOut,
        }
    };
    let (completed, timed_out, cancelled, execution_error) = match wait_outcome {
        ShellWaitOutcome::Exited(Ok(status)) => {
            (process.finish_after_exit(status).await, false, false, None)
        }
        ShellWaitOutcome::Exited(Err(error)) => (
            process.terminate().await,
            false,
            false,
            Some(error.to_string()),
        ),
        ShellWaitOutcome::TimedOut => (process.terminate().await, true, false, None),
        ShellWaitOutcome::Cancelled => (process.terminate().await, false, true, None),
    };
    let cleanup_error = completed.cleanup_error();
    let stdout_capture = completed.stdout;
    let stderr_capture = completed.stderr;
    let stdout = captured_shell_text(&stdout_capture.bytes, stdout_capture.truncated);
    let mut stderr = captured_shell_text(&stderr_capture.bytes, stderr_capture.truncated);
    for message in [
        timed_out.then_some("command timed out"),
        cancelled.then_some("command cancelled by user"),
        execution_error.as_deref(),
        cleanup_error.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(message);
    }

    Ok(CommandOutput {
        stdout,
        stderr,
        exit_code: completed.status.and_then(|value| value.code()),
        timed_out,
        cancelled,
        effect_started: true,
        cleanup_failed: execution_error.is_some() || cleanup_error.is_some(),
        stdout_truncated: stdout_capture.truncated,
        stderr_truncated: stderr_capture.truncated,
    })
}

fn resolve_shell_programs(
    shell: &crate::config::ShellConfig,
    family: ShellFamily,
    environment: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    if let Some(program) = &shell.program {
        return vec![program.to_string()];
    }
    match family {
        ShellFamily::PowerShell => default_powershell_programs(environment),
        ShellFamily::Bash => vec!["bash".to_string()],
    }
}

#[cfg(windows)]
fn default_powershell_programs(
    environment: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    if environment
        .keys()
        .any(|key| key.eq_ignore_ascii_case("PATH"))
    {
        vec!["pwsh".to_string(), "powershell".to_string()]
    } else {
        vec!["powershell".to_string()]
    }
}

#[cfg(not(windows))]
fn default_powershell_programs(
    _environment: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    vec!["powershell".to_string()]
}

fn retryable_windows_shell_spawn_error(error: &std::io::Error) -> bool {
    cfg!(windows) && error.raw_os_error().is_some()
}

#[cfg(all(test, windows))]
fn apply_shell_environment(command: &mut Command, shell: &crate::config::ShellConfig) {
    let environment = captured_process_environment(shell);
    apply_captured_shell_environment(command, &environment);
}

fn apply_captured_shell_environment(
    command: &mut Command,
    environment: &std::collections::HashMap<String, String>,
) {
    command.env_clear();
    command.envs(environment);
}

fn captured_shell_text(bytes: &[u8], truncated: bool) -> String {
    let mut text = decode_shell_bytes_for_display(bytes);
    if truncated {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("[shell stream capture truncated]");
    }
    text
}

fn decode_shell_bytes_for_display(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(value) => value,
        Err(_) => {
            let (decoded, _, had_errors) = SHIFT_JIS.decode(bytes);
            if had_errors {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                decoded.into_owned()
            }
        }
    }
}

#[cfg(test)]
fn shell_timeout_termination_plan() -> Vec<ShellTerminationStep> {
    process_tree_termination_plan()
}

#[cfg(test)]
type ShellTerminationStep = ProcessTerminationStep;

#[cfg(test)]
fn references_outside_workspace(workspace: &crate::workspace::Workspace, command: &str) -> bool {
    references_outside_workspace_from(workspace, &workspace.cwd, command)
}

fn references_outside_workspace_from(
    workspace: &crate::workspace::Workspace,
    workdir: &Utf8Path,
    command: &str,
) -> bool {
    if command.contains("..") {
        return true;
    }
    extract_absolute_paths(workdir, command)
        .into_iter()
        .any(|path| path_is_outside_writable_boundary(workspace, &path))
}

fn path_is_outside_writable_boundary(
    workspace: &crate::workspace::Workspace,
    path: &Utf8Path,
) -> bool {
    let inside = |root: &Utf8Path| PathGuard::security_path_is_within(path, root).unwrap_or(false);
    !inside(&workspace.root)
        && !workspace
            .path_policy
            .additional_write_roots
            .iter()
            .any(|root| inside(root))
}

fn extract_absolute_paths(workdir: &Utf8Path, command: &str) -> Vec<Utf8PathBuf> {
    let mut paths = Vec::new();
    let mut quoted_path_ranges = Vec::new();
    let quoted_values = [
        Regex::new(r#""([^"]+)""#).expect("double-quoted shell value regex"),
        Regex::new(r"'([^']+)'").expect("single-quoted shell value regex"),
    ];
    for quoted in &quoted_values {
        for capture in quoted.captures_iter(command) {
            let Some(candidate) = capture.get(1) else {
                continue;
            };
            let resolved = if cfg!(windows) {
                resolve_quoted_windows_path(workdir, candidate.as_str())
            } else {
                let path = Utf8PathBuf::from(candidate.as_str());
                path.is_absolute().then_some(path)
            };
            if let Some(path) = resolved {
                paths.push(path);
                quoted_path_ranges.push(candidate.start()..candidate.end());
            }
        }
    }

    if cfg!(windows) {
        let regex =
            Regex::new(r#"(?i)(?:[A-Z]:[\\/]|\\\\|//)[^\s"'|;,<>]+"#).expect("windows path regex");
        for candidate in regex.find_iter(command) {
            if quoted_path_ranges
                .iter()
                .any(|range| range.contains(&candidate.start()))
                || shell_path_candidate_is_inside_uri(command, candidate.start(), candidate.end())
            {
                continue;
            }
            let path = Utf8PathBuf::from(candidate.as_str().replace('/', "\\"));
            if path.is_absolute() {
                paths.push(path);
            }
        }
        let drive_relative =
            Regex::new(r#"(?i)(?:^|[\s"'|;=,()<>\[\]{}])([A-Z]:[^\\/\s"'|;,<>][^\s"'|;,<>]*)"#)
                .expect("windows drive-relative path regex");
        for capture in drive_relative.captures_iter(command) {
            let Some(candidate) = capture.get(1) else {
                continue;
            };
            if quoted_path_ranges
                .iter()
                .any(|range| range.contains(&candidate.start()))
                || shell_path_candidate_is_inside_uri(command, candidate.start(), candidate.end())
            {
                continue;
            }
            if let Some(path) = resolve_windows_drive_relative(workdir, candidate.as_str()) {
                paths.push(path);
            }
        }
    } else {
        let regex =
            Regex::new(r#"(?:^|[\s"'|;=,()<>\[\]{}])(/[^\s"'|;,<>]+)"#).expect("unix path regex");
        for capture in regex.captures_iter(command) {
            let Some(candidate) = capture.get(1) else {
                continue;
            };
            if quoted_path_ranges
                .iter()
                .any(|range| range.contains(&candidate.start()))
                || shell_path_candidate_is_inside_uri(command, candidate.start(), candidate.end())
            {
                continue;
            }
            let path = Utf8PathBuf::from(candidate.as_str());
            if path.is_absolute() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn resolve_quoted_windows_path(workdir: &Utf8Path, value: &str) -> Option<Utf8PathBuf> {
    let path = Utf8PathBuf::from(value.replace('/', "\\"));
    if path.is_absolute() {
        return Some(path);
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && !matches!(bytes[2], b'\\' | b'/')
    {
        return resolve_windows_drive_relative(workdir, value);
    }
    None
}

fn shell_path_candidate_is_inside_uri(command: &str, start: usize, end: usize) -> bool {
    let token_start = command[..start]
        .rfind(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '|' | ';' | ','))
        .map(|index| index + 1)
        .unwrap_or(0);
    let token_end = command[end..]
        .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '|' | ';' | ','))
        .map(|index| end + index)
        .unwrap_or(command.len());
    let token = &command[token_start..token_end];
    let candidate = &command[start..end];
    let candidate_bytes = candidate.as_bytes();
    let candidate_offset = start.saturating_sub(token_start);
    let candidate_end = end.saturating_sub(token_start);
    let marker_is_valid_scheme = |marker: usize| {
        if marker > candidate_end {
            return false;
        }
        let scheme_start = token[..marker]
            .rfind(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '+' | '-' | '.'))
            .map(|index| index + 1)
            .unwrap_or(0);
        let scheme = &token[scheme_start..marker];
        scheme.starts_with(|ch: char| ch.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    };
    let valid_markers = token
        .match_indices("://")
        .map(|(marker, _)| marker)
        .filter(|marker| marker_is_valid_scheme(*marker))
        .collect::<Vec<_>>();
    let windows_drive_with_forward_slashes = cfg!(windows)
        && candidate_bytes.len() >= 3
        && candidate_bytes[0].is_ascii_alphabetic()
        && candidate_bytes[1] == b':'
        && candidate_bytes[2] == b'/';
    if windows_drive_with_forward_slashes {
        let prior_uri = valid_markers
            .iter()
            .any(|marker| *marker < candidate_offset + 1);
        let continues_longer_scheme =
            candidate_offset > 0 && token.as_bytes()[candidate_offset - 1].is_ascii_alphanumeric();
        if !prior_uri && !continues_longer_scheme {
            return false;
        }
    }
    !valid_markers.is_empty()
}

fn resolve_windows_drive_relative(workdir: &Utf8Path, drive_relative: &str) -> Option<Utf8PathBuf> {
    let bytes = drive_relative.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let drive = &drive_relative[..2];
    let relative = drive_relative[2..].replace('/', "\\");
    let workdir_text = workdir.as_str();
    if workdir_text.len() >= 2 && workdir_text[..2].eq_ignore_ascii_case(drive) {
        return Some(workdir.join(relative));
    }
    Some(Utf8PathBuf::from(format!("{drive}\\{relative}")))
}

fn default_description(command: &str) -> String {
    let summary = command
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("run shell command");
    let shortened = if summary.len() > 80 {
        clip_text_with_ellipsis(summary, 80)
    } else {
        summary.to_string()
    };
    format!("Run shell command: {shortened}")
}

fn shell_permission_details(command: &str, workdir: &Utf8Path) -> Vec<String> {
    vec![
        format!("Command: {}", command.trim()),
        format!("Workdir: {workdir}"),
        "Workspace modes run this process in the native workspace-write OS sandbox; an approved elevation or Full Access runs it unrestricted under the current user account. The unelevated Windows backend uses advisory network controls rather than firewall enforcement."
            .to_string(),
    ]
}

#[cfg(test)]
fn shell_permission_risks(
    workspace: &crate::workspace::Workspace,
    command: &str,
) -> Vec<PermissionRisk> {
    let guarded = PathGuard::require_path(workspace, &workspace.cwd, AccessKind::Shell)
        .expect("test workspace cwd");
    shell_permission_risks_from(workspace, &guarded, command)
}

fn shell_permission_risks_from(
    workspace: &crate::workspace::Workspace,
    guarded_workdir: &GuardedPath,
    command: &str,
) -> Vec<PermissionRisk> {
    let mut risks = Vec::new();
    let references_network_path = shell_references_network_path(&guarded_workdir.absolute, command);
    if shell_has_delete_risk(command) {
        risks.push(PermissionRisk::DestructiveDelete);
    }
    if shell_has_move_risk(command) {
        risks.push(PermissionRisk::MoveOrRename);
    }
    if shell_has_network_risk(command) || references_network_path {
        risks.push(PermissionRisk::Network);
    }
    if shell_requires_external_connection_review(command) || references_network_path {
        risks.push(PermissionRisk::ExternalConnection);
    }
    if command_mentions_protected_target(workspace, guarded_workdir, command) {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    risks
}

pub(crate) fn process_argv_permission_risks(
    workspace: &crate::workspace::Workspace,
    guarded_workdir: &GuardedPath,
    argv: &[String],
    configured_instruction_files: &[Utf8PathBuf],
) -> Vec<PermissionRisk> {
    let command = argv.join(" ");
    let mut risks = shell_permission_risks_from(workspace, guarded_workdir, &command);
    let structured_paths = process_argv_path_candidates(&guarded_workdir.absolute, argv);
    if structured_paths.iter().any(|path| {
        PathGuard::require_path(workspace, path, AccessKind::Shell).is_ok_and(|guarded| {
            PathGuard::targets_protected_workspace_authority(&workspace.root, &guarded)
        })
    }) && !risks.contains(&PermissionRisk::ProtectedWorkspaceAuthority)
    {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    if command_mentions_configured_instruction_target(
        workspace,
        configured_instruction_files,
        guarded_workdir,
        &command,
    ) && !risks.contains(&PermissionRisk::ProtectedWorkspaceAuthority)
    {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    if configured_instruction_files.iter().any(|configured| {
        let candidate = if configured.is_absolute() {
            configured.clone()
        } else {
            workspace.root.join(configured)
        };
        crate::workspace::project::normalize_path(&workspace.root, &candidate).is_ok_and(
            |candidate| {
                structured_paths.iter().any(|path| {
                    PathGuard::stable_identity_key(path)
                        == PathGuard::stable_identity_key(&candidate)
                })
            },
        )
    }) && !risks.contains(&PermissionRisk::ProtectedWorkspaceAuthority)
    {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    risks
}

pub(crate) fn process_argv_references_outside_workspace(
    workspace: &crate::workspace::Workspace,
    guarded_workdir: &GuardedPath,
    argv: &[String],
) -> bool {
    references_outside_workspace_from(workspace, &guarded_workdir.absolute, &argv.join(" "))
        || process_argv_path_candidates(&guarded_workdir.absolute, argv)
            .iter()
            .any(|path| path_is_outside_writable_boundary(workspace, path))
}

fn process_argv_path_candidates(workdir: &Utf8Path, argv: &[String]) -> Vec<Utf8PathBuf> {
    let mut paths = extract_absolute_paths(workdir, &argv.join(" "));
    for argument in argv {
        for candidate in std::iter::once(argument.as_str())
            .chain(argument.split_once('=').map(|(_, value)| value))
        {
            let candidate = candidate.trim_matches(['"', '\'']);
            let resolved = if cfg!(windows) {
                resolve_quoted_windows_path(workdir, candidate)
            } else {
                let path = Utf8PathBuf::from(candidate);
                path.is_absolute().then_some(path)
            };
            if let Some(path) = resolved {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn shell_references_network_path(workdir: &Utf8Path, command: &str) -> bool {
    cfg!(windows)
        && (is_windows_unc_path(workdir)
            || extract_absolute_paths(workdir, command)
                .iter()
                .any(|path| is_windows_unc_path(path)))
}

fn is_windows_unc_path(path: &Utf8Path) -> bool {
    path.as_str().replace('/', "\\").starts_with("\\\\")
}

fn shell_has_delete_risk(command: &str) -> bool {
    command_tokens(command).iter().any(|token| {
        matches!(
            token.as_str(),
            "remove-item" | "rm" | "del" | "erase" | "rmdir" | "rd"
        )
    })
}

fn shell_has_move_risk(command: &str) -> bool {
    command_tokens(command).iter().any(|token| {
        matches!(
            token.as_str(),
            "mv" | "move" | "ren" | "rename" | "move-item" | "rename-item"
        )
    })
}

fn shell_has_network_risk(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if lower.contains("http://") || lower.contains("https://") {
        return true;
    }
    let tokens = command_tokens(command);
    if tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "curl" | "wget" | "invoke-webrequest" | "invoke-restmethod" | "iwr" | "irm"
        )
    }) {
        return true;
    }
    tokens.iter().any(|token| token == "git")
        && tokens
            .iter()
            .any(|token| matches!(token.as_str(), "fetch" | "pull" | "push" | "clone"))
}

fn shell_requires_external_connection_review(command: &str) -> bool {
    if shell_has_network_risk(command) {
        return true;
    }
    let tokens = command_tokens(command);
    let has_setup_action = tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "install"
                | "add"
                | "sync"
                | "fetch"
                | "update"
                | "upgrade"
                | "restore"
                | "download"
                | "pull"
                | "clone"
                | "ci"
                | "dlx"
        )
    });
    if !has_setup_action {
        return tokens.iter().any(|token| token == "npx")
            || (tokens.iter().any(|token| token == "uv")
                && tokens.iter().any(|token| token == "run"));
    }
    tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "pip"
                | "pip3"
                | "uv"
                | "npm"
                | "pnpm"
                | "yarn"
                | "poetry"
                | "pipenv"
                | "cargo"
                | "rustup"
                | "pyenv"
                | "conda"
                | "mamba"
                | "winget"
                | "choco"
                | "scoop"
                | "apt"
                | "apt-get"
                | "brew"
                | "git"
        )
    })
}

fn command_tokens(command: &str) -> Vec<String> {
    command
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn command_mentions_protected_target(
    workspace: &crate::workspace::Workspace,
    guarded_workdir: &GuardedPath,
    command: &str,
) -> bool {
    if PathGuard::targets_protected_workspace_authority(&workspace.root, guarded_workdir) {
        return true;
    }
    let lower = command.to_ascii_lowercase();
    if [
        "agents.md",
        "agent.md",
        "agents.local.md",
        "claude.md",
        "skill.md",
        ".moyai/rules",
        ".moyai\\rules",
    ]
    .into_iter()
    .any(|needle| lower.contains(needle))
    {
        return true;
    }
    extract_absolute_paths(&guarded_workdir.absolute, command)
        .into_iter()
        .any(|path| {
            PathGuard::require_path(workspace, &path, AccessKind::Shell).is_ok_and(|guarded| {
                PathGuard::targets_protected_workspace_authority(&workspace.root, &guarded)
            })
        })
}

fn command_mentions_configured_instruction_target(
    workspace: &crate::workspace::Workspace,
    configured_files: &[Utf8PathBuf],
    guarded_workdir: &GuardedPath,
    command: &str,
) -> bool {
    let lower = command.replace('/', "\\").to_ascii_lowercase();
    let absolute_paths = extract_absolute_paths(&guarded_workdir.absolute, command);
    configured_files.iter().any(|configured| {
        let candidate = if configured.is_absolute() {
            configured.clone()
        } else {
            workspace.root.join(configured)
        };
        let Ok(candidate) = crate::workspace::project::normalize_path(&workspace.root, &candidate)
        else {
            return false;
        };
        absolute_paths.iter().any(|path| {
            PathGuard::stable_identity_key(path) == PathGuard::stable_identity_key(&candidate)
        }) || lower.contains(&candidate.as_str().replace('/', "\\").to_ascii_lowercase())
            || candidate
                .file_name()
                .is_some_and(|name| lower.contains(&name.to_ascii_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use tokio_util::sync::CancellationToken;

    use crate::config::ResolvedConfig;
    use crate::workspace::WorkspaceDiscovery;

    #[test]
    fn shell_output_is_factual_without_retry_coaching() {
        let output = super::format_shell_output_for_display(
            "Get-Process",
            "powershell 123",
            "error",
            Some(1),
            false,
            false,
        );
        assert!(output.contains("Exit code: 1"));
        assert!(output.contains("Stdout:\npowershell 123"));
        assert!(!output.contains("Recovery:"));
        assert!(!output.contains("retry"));
    }

    #[test]
    fn protected_effect_temp_access_denial_is_classified_narrowly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let workspace_write = crate::tool::os_sandbox::ProcessSandboxPlan::for_access_mode(
            crate::config::AccessMode::Default,
            &workspace,
        )
        .expect("workspace-write plan");
        let make_output = |stdout: &str, stderr: &str| super::CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code: Some(1),
            timed_out: false,
            cancelled: false,
            effect_started: true,
            cleanup_failed: false,
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let signature = "PermissionError: [WinError 5] Access is denied: 'C:\\Temp\\moyai-sandbox-effect-ABC\\pytest'";

        let classified = super::classify_shell_sandbox_failure_hint(
            &workspace_write,
            &make_output("", signature),
        );
        assert_eq!(
            classified,
            Some(super::ShellSandboxFailureHint::WorkspaceWriteEffectTempAccessDenied)
        );
        assert_eq!(
            serde_json::to_value(classified).expect("serialize hint"),
            serde_json::json!("workspace_write_effect_temp_access_denied")
        );
        assert_eq!(
            super::classify_shell_sandbox_failure_hint(
                &workspace_write,
                &make_output(
                    "permissionERROR: [WINerror 5] C:\\Temp\\MOYAI-SANDBOX-EFFECT-mixed",
                    "",
                ),
            ),
            Some(super::ShellSandboxFailureHint::WorkspaceWriteEffectTempAccessDenied)
        );

        for output in [
            make_output("", "ordinary command failure"),
            make_output("", "PermissionError: [WinError 5] Access is denied"),
            make_output("", "C:\\Temp\\moyai-sandbox-effect-ABC\\pytest"),
            make_output(
                "PermissionError: [WinError 5] Access is denied",
                "C:\\Temp\\moyai-sandbox-effect-ABC\\pytest",
            ),
            make_output(
                "",
                "PermissionError: [WinError 5] Access is denied\nC:\\Temp\\moyai-sandbox-effect-ABC\\pytest",
            ),
        ] {
            assert_eq!(
                super::classify_shell_sandbox_failure_hint(&workspace_write, &output),
                None
            );
        }

        let mut exit_zero = make_output("", signature);
        exit_zero.exit_code = Some(0);
        let mut no_exit_code = make_output("", signature);
        no_exit_code.exit_code = None;
        let mut not_started = make_output("", signature);
        not_started.effect_started = false;
        let mut timed_out = make_output("", signature);
        timed_out.timed_out = true;
        let mut cancelled = make_output("", signature);
        cancelled.cancelled = true;
        let mut cleanup_failed = make_output("", signature);
        cleanup_failed.cleanup_failed = true;
        for output in [
            exit_zero,
            no_exit_code,
            not_started,
            timed_out,
            cancelled,
            cleanup_failed,
        ] {
            assert_eq!(
                super::classify_shell_sandbox_failure_hint(&workspace_write, &output),
                None
            );
        }
        assert_eq!(
            super::classify_shell_sandbox_failure_hint(
                &crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted,
                &make_output("", signature),
            ),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn default_windows_powershell_uses_lazy_path_resolution_and_preserves_override() {
        let environment = std::collections::HashMap::from([(
            "Path".to_string(),
            r"C:\first;\\unreachable\unused".to_string(),
        )]);
        let mut shell = ResolvedConfig::default().shell;

        assert_eq!(
            super::resolve_shell_programs(
                &shell,
                crate::config::ShellFamily::PowerShell,
                &environment,
            ),
            vec!["pwsh".to_string(), "powershell".to_string()]
        );

        shell.program = Some(Utf8PathBuf::from("explicit-shell.exe"));
        assert_eq!(
            super::resolve_shell_programs(
                &shell,
                crate::config::ShellFamily::PowerShell,
                &environment,
            ),
            vec!["explicit-shell.exe".to_string()]
        );
    }

    #[test]
    fn network_urls_are_not_classified_as_outside_workspace_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        for command in [
            "curl.exe http://127.0.0.1:18945/health",
            "Invoke-WebRequest https://example.com/C:/artifact.json",
        ] {
            assert!(!super::references_outside_workspace(&workspace, command));
            let risks = super::shell_permission_risks(&workspace, command);
            assert!(risks.contains(&crate::tool::PermissionRisk::Network));
            assert!(risks.contains(&crate::tool::PermissionRisk::ExternalConnection));
        }
    }

    #[test]
    fn shell_confirmation_discloses_execution_boundary() {
        let details =
            super::shell_permission_details("Get-Date", camino::Utf8Path::new("C:/workspace"));
        assert_eq!(details[0], "Command: Get-Date");
        assert_eq!(details[1], "Workdir: C:/workspace");
        assert!(details[2].contains("current user account"));
        assert!(details[2].contains("workspace-write OS sandbox"));
        assert!(details[2].contains("advisory network controls"));
    }

    #[test]
    fn shell_permission_targets_include_detected_absolute_effect_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let external = if cfg!(windows) {
            Utf8PathBuf::from(r"C:\outside\pytest-target")
        } else {
            Utf8PathBuf::from("/outside/pytest-target")
        };
        let input: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": format!("Remove-Item -Recurse -Force '{}'", external)
        }))
        .expect("shell input");

        let intent =
            super::shell_permission_intent(&workspace, &config, &input).expect("permission intent");

        assert_eq!(intent.targets.first(), Some(&workspace.cwd));
        assert!(intent.targets.contains(&external));
        assert!(intent.outside_workspace);
        assert!(
            intent
                .risks
                .contains(&crate::tool::PermissionRisk::DestructiveDelete)
        );
    }

    #[cfg(windows)]
    #[test]
    fn shell_permission_targets_extract_nested_quoted_windows_path_with_spaces() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let external = Utf8PathBuf::from(r"C:\outside folder\pytest target.txt");
        let input: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": format!(
                "powershell -NoProfile -Command \"Remove-Item -LiteralPath '{}'\"",
                external
            )
        }))
        .expect("shell input");

        let intent =
            super::shell_permission_intent(&workspace, &config, &input).expect("permission intent");

        assert_eq!(intent.targets, vec![workspace.cwd.clone(), external]);
    }

    #[cfg(windows)]
    #[test]
    fn shell_permission_targets_extract_multiple_nested_absolute_paths_exactly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let source = Utf8PathBuf::from(r"C:\source folder\input.txt");
        let destination = Utf8PathBuf::from(r"D:\archive folder\output.txt");
        let input: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": format!(
                "powershell -NoProfile -Command \"Copy-Item -LiteralPath '{}' -Destination '{}'\"",
                source, destination
            )
        }))
        .expect("shell input");

        let intent =
            super::shell_permission_intent(&workspace, &config, &input).expect("permission intent");

        assert_eq!(
            intent.targets,
            vec![workspace.cwd.clone(), source, destination]
        );
    }

    #[cfg(windows)]
    #[test]
    fn shell_permission_targets_extract_nested_quoted_unc_path_with_spaces() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let external = Utf8PathBuf::from(r"\\server\shared folder\pytest target.txt");
        let input: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": format!(
                "powershell -NoProfile -Command \"Remove-Item -LiteralPath '{}'\"",
                external
            )
        }))
        .expect("shell input");

        let intent =
            super::shell_permission_intent(&workspace, &config, &input).expect("permission intent");

        assert_eq!(intent.targets, vec![workspace.cwd.clone(), external]);
    }

    #[test]
    fn explicit_sandbox_elevation_requires_justification() {
        let spec = crate::tool::registry::Tool::spec(&super::ShellTool);
        assert_eq!(
            spec.input_schema["properties"]["sandbox_permissions"]["enum"],
            serde_json::json!(["use_default", "require_escalated"])
        );
        assert_eq!(
            spec.input_schema["properties"]["justification"]["type"],
            "string"
        );
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let default_input: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": "Get-Date"
        }))
        .expect("default shell input");
        assert!(!super::shell_requested_elevation(&default_input).expect("default plan"));

        let missing: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": "Get-Date",
            "sandbox_permissions": "require_escalated"
        }))
        .expect("escalated shell input");
        assert!(super::shell_requested_elevation(&missing).is_err());

        let justified: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": "Get-Date",
            "workdir": root,
            "sandbox_permissions": "require_escalated",
            "justification": "needs an exact external effect"
        }))
        .expect("justified shell input");
        assert!(super::shell_requested_elevation(&justified).expect("elevated plan"));
        let intent = super::shell_permission_intent(&workspace, &config, &justified)
            .expect("explicit elevation intent");
        assert!(intent.outside_workspace);
        assert!(intent.risks.is_empty());
        assert!(intent.details.iter().any(|detail| {
            detail == "Requested sandbox elevation: needs an exact external effect"
        }));
    }

    #[test]
    fn configured_instruction_command_is_routed_to_authority_review() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut config = ResolvedConfig::default();
        config.instructions.additional_files = vec![Utf8PathBuf::from("policy.md")];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let input: super::ShellInput = serde_json::from_value(serde_json::json!({
            "command": "Set-Content -LiteralPath policy.md -Value changed"
        }))
        .expect("shell input");

        let intent = super::shell_permission_intent(&workspace, &config, &input)
            .expect("configured instruction intent");
        assert!(
            intent
                .risks
                .contains(&crate::tool::PermissionRisk::ProtectedWorkspaceAuthority)
        );
    }

    #[test]
    fn move_aliases_at_command_boundaries_are_routed_to_review() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");

        for command in [
            "mv -Force source.txt target.txt",
            "\tmv source.txt target.txt",
            "Write-Output ready;ren old.txt new.txt",
            "Write-Output ready\nRename-Item old.txt new.txt",
        ] {
            assert!(
                super::shell_permission_risks(&workspace, command)
                    .contains(&crate::tool::PermissionRisk::MoveOrRename),
                "command was not classified: {command}"
            );
        }
        assert!(!super::shell_has_move_risk("Write-Output movement"));
    }

    #[test]
    fn delete_and_network_aliases_with_non_space_boundaries_are_routed_to_review() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");

        let delete_risks = super::shell_permission_risks(&workspace, "rm\told.txt");
        assert!(delete_risks.contains(&crate::tool::PermissionRisk::DestructiveDelete));
        for command in [
            "curl\texample.com",
            "wget\texample.com",
            "git\tpush origin main",
        ] {
            let risks = super::shell_permission_risks(&workspace, command);
            assert!(
                risks.contains(&crate::tool::PermissionRisk::Network)
                    && risks.contains(&crate::tool::PermissionRisk::ExternalConnection),
                "command was not classified: {command}"
            );
        }
        assert!(!super::shell_has_delete_risk("Write-Output rmdirname"));
        assert!(!super::shell_has_network_risk("Write-Output curling"));
    }

    #[cfg(windows)]
    #[test]
    fn structured_formatter_argv_preserves_spaces_when_classifying_outside_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fixture = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 fixture");
        let root = fixture.join("repo");
        let outside = fixture.join("repo escaped");
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::create_dir_all(&outside).expect("outside root");
        let executable = outside.join("formatter.exe");
        std::fs::write(&executable, b"fixture").expect("outside formatter fixture");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let guarded = crate::workspace::PathGuard::require_path(
            &workspace,
            &root,
            crate::workspace::AccessKind::Shell,
        )
        .expect("guarded workdir");

        assert!(super::process_argv_references_outside_workspace(
            &workspace,
            &guarded,
            &[executable.to_string()]
        ));
    }

    #[test]
    fn shell_timeout_termination_starts_with_process_tree_kill() {
        assert_eq!(
            super::shell_timeout_termination_plan(),
            vec![
                super::ShellTerminationStep::ProcessTreeKill,
                super::ShellTerminationStep::ParentStartKill,
                super::ShellTerminationStep::WaitForParent,
            ]
        );
    }

    #[tokio::test]
    async fn pre_cancelled_shell_does_not_start_an_effect() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let output = super::execute_shell_command(
            &config.shell,
            &root,
            pre_cancelled_write_command(),
            5_000,
            1_024,
            cancel,
            &crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted,
        )
        .await
        .expect("pre-cancel output");

        assert!(output.cancelled);
        assert!(!output.effect_started);
        assert!(!root.join("pre-cancelled.txt").exists());
    }

    #[tokio::test]
    async fn shell_stream_capture_is_bounded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let output = super::execute_shell_command(
            &config.shell,
            &root,
            large_output_command(),
            5_000,
            32,
            CancellationToken::new(),
            &crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted,
        )
        .await
        .expect("bounded shell output");

        assert!(output.effect_started);
        assert!(output.stdout_truncated);
        assert!(output.stdout.ends_with("[shell stream capture truncated]"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn workspace_write_shell_dispatch_allows_workspace_and_denies_dynamic_outside_write() {
        std::fs::create_dir_all("target").expect("target directory");
        let temp = tempfile::Builder::new()
            .prefix("moyai-shell-sandbox-")
            .tempdir_in("target")
            .expect("tempdir");
        let fixture = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let workspace_root = fixture.join("workspace");
        let outside_root = fixture.join("outside");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        std::fs::create_dir_all(&outside_root).expect("outside");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let plan = crate::tool::os_sandbox::ProcessSandboxPlan::for_access_mode(
            crate::config::AccessMode::Default,
            &workspace,
        )
        .expect("workspace plan");
        let inside = workspace_root.join("inside.txt");
        let outside = outside_root.join("outside.txt");
        let command = format!(
            "Set-Content -LiteralPath '{}' -Value inside; try {{ Set-Content -LiteralPath '{}' -Value outside -ErrorAction Stop; Write-Output outside-allowed }} catch {{ Write-Output outside-denied }}",
            inside.as_str().replace('\'', "''"),
            outside.as_str().replace('\'', "''")
        );

        let output = super::execute_shell_command(
            &config.shell,
            &workspace_root,
            &command,
            30_000,
            8_192,
            CancellationToken::new(),
            &plan,
        )
        .await
        .expect("workspace-write shell");

        assert_eq!(output.exit_code, Some(0));
        assert!(inside.exists());
        assert!(!outside.exists());
        assert!(output.stdout.contains("outside-denied"));
        assert!(!output.cleanup_failed);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn installed_default_pwsh_supports_conjunction_in_both_process_profiles() {
        let pwsh_probe = std::process::Command::new("pwsh")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "exit 0",
            ])
            .status();
        if !matches!(pwsh_probe, Ok(status) if status.success()) {
            return;
        }
        std::fs::create_dir_all("target").expect("target directory");
        let temp = tempfile::Builder::new()
            .prefix("moyai-shell-pwsh-")
            .tempdir_in("target")
            .expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let workspace_plan = crate::tool::os_sandbox::ProcessSandboxPlan::for_access_mode(
            crate::config::AccessMode::Default,
            &workspace,
        )
        .expect("workspace plan");

        for (label, plan) in [
            ("workspace-write", workspace_plan),
            (
                "unrestricted",
                crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted,
            ),
        ] {
            let output = super::execute_shell_command(
                &config.shell,
                &workspace_root,
                &format!("cmd.exe /d /c exit 0 && Write-Output {label}-pwsh-ok"),
                30_000,
                8_192,
                CancellationToken::new(),
                &plan,
            )
            .await
            .expect("pwsh shell execution");

            assert_eq!(output.exit_code, Some(0), "stderr={}", output.stderr);
            assert!(
                output.stdout.contains(&format!("{label}-pwsh-ok")),
                "stdout={}",
                output.stdout
            );
            assert!(!output.cleanup_failed);
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unusable_pwsh_candidate_falls_back_before_effect_in_both_process_profiles() {
        std::fs::create_dir_all("target").expect("target directory");
        let temp = tempfile::Builder::new()
            .prefix("moyai-shell-pwsh-fallback-")
            .tempdir_in("target")
            .expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let unusable = workspace_root.join("pwsh.exe");
        std::fs::write(&unusable, b"not a Windows executable").expect("invalid pwsh fixture");
        let config = ResolvedConfig::default();
        let environment = crate::tool::sandbox_process::captured_process_environment(&config.shell);
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let workspace_plan = crate::tool::os_sandbox::ProcessSandboxPlan::for_access_mode(
            crate::config::AccessMode::Default,
            &workspace,
        )
        .expect("workspace plan");

        for (label, plan) in [
            ("workspace-write", workspace_plan),
            (
                "unrestricted",
                crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted,
            ),
        ] {
            let output = super::execute_shell_command_with_programs(
                &config.shell,
                &workspace_root,
                &format!("Write-Output {label}-fallback-ok"),
                30_000,
                8_192,
                CancellationToken::new(),
                &plan,
                crate::config::ShellFamily::PowerShell,
                environment.clone(),
                vec![unusable.to_string(), "powershell".to_string()],
            )
            .await
            .expect("pre-effect launch failure should use the next candidate");

            assert_eq!(output.exit_code, Some(0), "stderr={}", output.stderr);
            assert!(
                output.stdout.contains(&format!("{label}-fallback-ok")),
                "stdout={}",
                output.stdout
            );
            assert!(!output.cleanup_failed);
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn launched_candidate_is_never_replayed_after_a_nonzero_exit() {
        std::fs::create_dir_all("target").expect("target directory");
        let temp = tempfile::Builder::new()
            .prefix("moyai-shell-no-replay-")
            .tempdir_in("target")
            .expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let environment = crate::tool::sandbox_process::captured_process_environment(&config.shell);
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let workspace_plan = crate::tool::os_sandbox::ProcessSandboxPlan::for_access_mode(
            crate::config::AccessMode::Default,
            &workspace,
        )
        .expect("workspace plan");

        for (label, plan) in [
            ("workspace-write", workspace_plan),
            (
                "unrestricted",
                crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted,
            ),
        ] {
            let first_marker = workspace_root.join(format!("{label}-first.txt"));
            let replay_marker = workspace_root.join(format!("{label}-replayed.txt"));
            let quote = |path: &camino::Utf8Path| path.as_str().replace('\'', "''");
            let command = format!(
                "if ($PSVersionTable.PSEdition -eq 'Desktop') {{ Set-Content -LiteralPath '{}' -Value first; exit 23 }} else {{ Set-Content -LiteralPath '{}' -Value replayed; exit 0 }}",
                quote(&first_marker),
                quote(&replay_marker)
            );

            let output = super::execute_shell_command_with_programs(
                &config.shell,
                &workspace_root,
                &command,
                30_000,
                8_192,
                CancellationToken::new(),
                &plan,
                crate::config::ShellFamily::PowerShell,
                environment.clone(),
                vec!["powershell".to_string(), "pwsh".to_string()],
            )
            .await
            .expect("a launched candidate's nonzero exit is a completed execution");

            assert_eq!(output.exit_code, Some(23), "stderr={}", output.stderr);
            assert!(first_marker.exists(), "first candidate did not run");
            assert!(
                !replay_marker.exists(),
                "second candidate replayed an already-started effect"
            );
            assert!(!output.cleanup_failed);
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unrestricted_shell_dispatch_can_write_outside_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fixture = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let workspace_root = fixture.join("workspace");
        let outside_root = fixture.join("outside");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        std::fs::create_dir_all(&outside_root).expect("outside");
        let config = ResolvedConfig::default();
        let outside = outside_root.join("outside.txt");
        let host_environment =
            crate::tool::sandbox_process::captured_process_environment(&config.shell);
        let host_temp = host_environment
            .get("TEMP")
            .expect("default Windows shell environment includes TEMP");
        let host_tmp = host_environment
            .get("TMP")
            .expect("default Windows shell environment includes TMP");
        let command = format!(
            "Set-Content -LiteralPath '{}' -Value outside; Write-Output \"TEMP=$env:TEMP\"; Write-Output \"TMP=$env:TMP\"",
            outside.as_str().replace('\'', "''")
        );
        let output = super::execute_shell_command(
            &config.shell,
            &workspace_root,
            &command,
            30_000,
            8_192,
            CancellationToken::new(),
            &crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted,
        )
        .await
        .expect("unrestricted shell");

        assert_eq!(output.exit_code, Some(0));
        assert!(outside.exists());
        assert!(
            output
                .stdout
                .lines()
                .any(|line| line == format!("TEMP={host_temp}")),
            "Full Access changed the host TEMP: {}",
            output.stdout
        );
        assert!(
            output
                .stdout
                .lines()
                .any(|line| line == format!("TMP={host_tmp}")),
            "Full Access changed the host TMP: {}",
            output.stdout
        );
        assert!(!output.stdout.contains("moyai-sandbox-effect-"));
        assert!(!output.cleanup_failed);
    }

    #[tokio::test]
    async fn no_process_admission_never_spawns_a_shell() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let marker = root.join("must-not-start.txt");
        let config = ResolvedConfig::default();
        let command = if cfg!(windows) {
            format!("Set-Content -LiteralPath '{}' -Value started", marker)
        } else {
            format!("printf started > '{}'", marker)
        };
        let result = super::execute_shell_command(
            &config.shell,
            &root,
            &command,
            5_000,
            1_024,
            CancellationToken::new(),
            &crate::tool::os_sandbox::ProcessSandboxPlan::NoProcess,
        )
        .await;
        let Err(error) = result else {
            panic!("no-process admission must reject shell dispatch");
        };

        assert!(matches!(
            error,
            crate::error::ToolError::SandboxExecution(
                crate::tool::sandbox_process::SandboxExecutionError::InvalidProfile(_)
            )
        ));
        assert!(!marker.exists());
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn workspace_shell_fails_closed_without_a_native_backend() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir(&root).expect("workspace root");
        let marker = root.join("must-not-start.txt");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace discovery");
        let plan = crate::tool::os_sandbox::ProcessSandboxPlan::for_access_mode(
            crate::config::AccessMode::Default,
            &workspace,
        )
        .expect("workspace sandbox plan");
        let error = super::execute_shell_command(
            &config.shell,
            &root,
            &format!("printf started > '{}'", marker),
            5_000,
            1_024,
            CancellationToken::new(),
            &plan,
        )
        .await
        .err()
        .expect("unsupported workspace sandbox must fail closed");

        assert!(matches!(
            error,
            crate::error::ToolError::SandboxExecution(
                crate::tool::sandbox_process::SandboxExecutionError::UnsupportedPlatform
            )
        ));
        assert!(!marker.exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_shell_environment_is_allowlist_only_without_encoding_injection() {
        let config = ResolvedConfig::default();
        let system_root = std::env::var("SystemRoot").expect("SystemRoot");
        let executable =
            Utf8PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = tokio::process::Command::new(executable);
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$secret=[Environment]::GetEnvironmentVariable('MOYAI_FORBIDDEN_TEST'); $utf8=[Environment]::GetEnvironmentVariable('PYTHONUTF8'); [Console]::Out.Write(\"$secret|$utf8\")",
        ]);
        command.env("MOYAI_FORBIDDEN_TEST", "secret");
        command.env("PYTHONUTF8", "forced");
        super::apply_shell_environment(&mut command, &config.shell);
        let output = command.output().await.expect("run filtered child");
        let stdout = String::from_utf8(output.stdout).expect("utf8 output");
        assert!(output.status.success());
        assert_eq!(stdout, "|");
    }

    #[cfg(windows)]
    fn pre_cancelled_write_command() -> &'static str {
        "Set-Content -LiteralPath pre-cancelled.txt -Value changed"
    }

    #[cfg(not(windows))]
    fn pre_cancelled_write_command() -> &'static str {
        "printf changed > pre-cancelled.txt"
    }

    #[cfg(windows)]
    fn large_output_command() -> &'static str {
        "[Console]::Out.Write(('x' * 2048))"
    }

    #[cfg(not(windows))]
    fn large_output_command() -> &'static str {
        "yes x | head -c 2048 | tr -d '\\n'"
    }
}
