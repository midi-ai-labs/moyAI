use std::collections::HashMap;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use encoding_rs::SHIFT_JIS;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

use crate::config::ShellFamily;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::process::ManagedProcess;
#[cfg(test)]
use crate::tool::process::{ProcessTerminationStep, process_tree_termination_plan};
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard, is_protected_workspace_authority_path};

#[derive(Debug, Deserialize)]
pub struct ShellInput {
    pub command: String,
    pub workdir: Option<Utf8PathBuf>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Default)]
pub struct ShellTool;

#[async_trait(?Send)]
impl Tool for ShellTool {
    fn spec(&self) -> ToolSpec {
        let description = if cfg!(windows) {
            "Run a PowerShell command with the current user account. Shell side effects have no typed file-change owner, so every edit baseline for the current session is invalidated before execution."
        } else {
            "Run a bash command with the current user account. Shell side effects have no typed file-change owner, so every edit baseline for the current session is invalidated before execution."
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
                    "description": { "type": "string" }
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
        let requested_workdir = input.workdir.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded =
            PathGuard::require_path(ctx.workspace, &requested_workdir, AccessKind::Shell)?;
        if !guarded.absolute.is_dir() {
            return Err(ToolError::Message(format!(
                "shell workdir `{}` is not a directory",
                guarded.absolute
            )));
        }
        let outside_workspace = (!guarded.inside_workspace && !guarded.trusted_external)
            || references_outside_workspace_from(ctx.workspace, &guarded.absolute, &input.command);
        let description = if input.description.trim().is_empty() {
            default_description(&input.command)
        } else {
            input.description.clone()
        };
        let risks = shell_permission_risks_from(ctx.workspace, &guarded.absolute, &input.command);
        let effect_admission = ctx
            .confirm_if_needed_with_details(
                AccessKind::Shell,
                description.clone(),
                shell_permission_details(&input.command, &guarded.absolute),
                vec![guarded.absolute.clone()],
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
                "success": output.exit_code == Some(0) && !output.timed_out && !output.cancelled,
                "change_evidence": {
                    "status": change_evidence_status,
                    "effects_unknown": output.effect_started,
                    "session_edit_baselines_invalidated": effect_may_start,
                }
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: preview.internal_file_lease,
        })
    }
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
    stdout_truncated: bool,
    stderr_truncated: bool,
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
) -> Result<CommandOutput, ToolError> {
    if cancel.is_cancelled() {
        return Ok(CommandOutput {
            stdout: String::new(),
            stderr: "command cancelled by user".to_string(),
            exit_code: None,
            timed_out: false,
            cancelled: true,
            effect_started: false,
            stdout_truncated: false,
            stderr_truncated: false,
        });
    }
    let family = shell.family.unwrap_or(if cfg!(windows) {
        ShellFamily::PowerShell
    } else {
        ShellFamily::Bash
    });
    let mut command = if let Some(program) = &shell.program {
        Command::new(program)
    } else if matches!(family, ShellFamily::PowerShell) {
        Command::new("powershell")
    } else {
        Command::new("bash")
    };
    match family {
        ShellFamily::PowerShell => {
            command.args(["-NoProfile", "-Command", command_text]);
        }
        ShellFamily::Bash => {
            command.args(["-lc", command_text]);
        }
    }
    command.current_dir(workdir.as_std_path());
    apply_shell_environment(&mut command, shell);
    let mut process = ManagedProcess::spawn(command, shell.hide_windows, max_output_bytes).await?;
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
        stdout_truncated: stdout_capture.truncated,
        stderr_truncated: stderr_capture.truncated,
    })
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

fn apply_shell_environment(command: &mut Command, shell: &crate::config::ShellConfig) {
    let mut captured = HashMap::new();
    for key in &shell.env_allowlist {
        if let Some(value) = std::env::var_os(key) {
            captured.insert(key.clone(), value);
        }
    }
    command.env_clear();
    for key in &shell.env_allowlist {
        if let Some(value) = captured.get(key) {
            command.env(key, value);
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
        .any(|path| {
            !path.starts_with(&workspace.root)
                && !workspace
                    .path_policy
                    .additional_write_roots
                    .iter()
                    .any(|root| path.starts_with(root))
        })
}

fn extract_absolute_paths(workdir: &Utf8Path, command: &str) -> Vec<Utf8PathBuf> {
    let mut paths = Vec::new();
    let mut quoted_path_ranges = Vec::new();
    let quoted = Regex::new(r#""([^"]+)"|'([^']+)'"#).expect("quoted shell value regex");
    for capture in quoted.captures_iter(command) {
        let Some(candidate) = capture.get(1).or_else(|| capture.get(2)) else {
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
        "Shell runs with the current user account; permission review is risk classification, not an OS filesystem sandbox."
            .to_string(),
    ]
}

#[cfg(test)]
fn shell_permission_risks(
    workspace: &crate::workspace::Workspace,
    command: &str,
) -> Vec<PermissionRisk> {
    shell_permission_risks_from(workspace, &workspace.cwd, command)
}

fn shell_permission_risks_from(
    workspace: &crate::workspace::Workspace,
    workdir: &Utf8Path,
    command: &str,
) -> Vec<PermissionRisk> {
    let mut risks = Vec::new();
    let references_network_path = shell_references_network_path(workdir, command);
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
    if command_mentions_protected_target(workspace, workdir, command) {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    risks
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
    let lower = command.to_ascii_lowercase();
    ["remove-item", "rm ", "del ", "erase ", "rmdir ", "rd "]
        .into_iter()
        .any(|needle| lower.contains(needle))
}

fn shell_has_move_risk(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "move-item",
        "rename-item",
        " mv ",
        "move ",
        " ren ",
        "rename ",
    ]
    .into_iter()
    .any(|needle| lower.contains(needle))
}

fn shell_has_network_risk(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "http://",
        "https://",
        "curl ",
        "wget ",
        "invoke-webrequest",
        "invoke-restmethod",
        "iwr ",
        "irm ",
        "git fetch",
        "git pull",
        "git push",
        "git clone",
    ]
    .into_iter()
    .any(|needle| lower.contains(needle))
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
    workdir: &Utf8Path,
    command: &str,
) -> bool {
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
    extract_absolute_paths(workdir, command)
        .into_iter()
        .any(|path| is_protected_workspace_authority_path(&workspace.root, &path))
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
    fn network_urls_are_not_classified_as_outside_workspace_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
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
        assert!(details[2].contains("not an OS filesystem sandbox"));
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
        )
        .await
        .expect("bounded shell output");

        assert!(output.effect_started);
        assert!(output.stdout_truncated);
        assert!(output.stdout.ends_with("[shell stream capture truncated]"));
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
