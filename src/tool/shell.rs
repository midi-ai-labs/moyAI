use std::collections::HashMap;
use std::fs;
use std::process::ExitStatus;
use std::process::Stdio;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use encoding_rs::SHIFT_JIS;
use ignore::WalkBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

use crate::agent::language_evidence::{
    language_command_inherits_utf8_bootstrap, language_command_test_or_verification_io_evidence,
    language_command_text_io_surface_evidence, language_python_utf8_correction_applies,
    language_runtime_execution_io_evidence,
};
use crate::config::ShellFamily;
use crate::edit::path_for_change_storage;
use crate::error::ToolError;
use crate::session::ChangeRepository;
use crate::tool::context::ToolContext;
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
            "Run a shell command in the workspace. On Windows this tool executes PowerShell, so send raw PowerShell syntax only."
        } else {
            "Run a shell command in the workspace. On Unix this tool executes bash."
        };
        ToolSpec {
            name: ToolName::Shell,
            description,
            input_schema: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {
                        "type": "string",
                        "description": if cfg!(windows) {
                            "Raw PowerShell command text. Do not use bash operators like `&&`, stderr redirection such as `2>&1`, or heredocs, and do not prefix with `powershell -Command`."
                        } else {
                            "Raw shell command text."
                        }
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Optional working directory, preferably relative to the workspace root."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional timeout in milliseconds."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional short summary of what the command is doing. If omitted, moyai will derive one."
                    }
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
        let family = ctx.config.shell.family.unwrap_or(if cfg!(windows) {
            ShellFamily::PowerShell
        } else {
            ShellFamily::Bash
        });
        let requested_workdir = input.workdir.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded =
            PathGuard::require_path(ctx.workspace, &requested_workdir, AccessKind::Shell)?;
        if let Some(violation) = shell_contract_violation(&input.command, family) {
            return Ok(shell_contract_violation_result(&input.command, violation));
        }
        let outside_workspace = (!guarded.inside_workspace && !guarded.trusted_external)
            || references_outside_workspace(ctx.workspace, &input.command);
        let description = if input.description.trim().is_empty() {
            default_description(&input.command)
        } else {
            input.description.clone()
        };
        let encoding_review = command_text_encoding_review(&input.command, family);
        let risks = shell_permission_risks(ctx.workspace, &input.command);
        ctx.confirm_if_needed_with_details(
            AccessKind::Shell,
            description.clone(),
            shell_permission_details(&input.command, guarded.absolute.as_path()),
            vec![guarded.absolute.clone()],
            outside_workspace,
            risks,
        )?;

        let timeout_ms = input
            .timeout_ms
            .unwrap_or(ctx.config.shell.default_timeout_ms)
            .min(ctx.config.shell.max_timeout_ms);
        let before = snapshot_workspace(ctx.workspace)?;
        let output = execute_shell_command(
            &ctx.config.shell,
            &guarded.absolute,
            &input.command,
            timeout_ms,
            ctx.cancel.clone(),
        )
        .await?;
        let after = snapshot_workspace(ctx.workspace)?;
        let shell_changes = build_shell_changes(&ctx, before, after)?;
        let baseline_snapshot = snapshot_and_sync_shell_change_set(
            &ctx.services.edit_safety,
            ctx.session.session.id,
            &shell_changes,
        )?;
        let changes = shell_changes.changes;
        let change_ids = match ctx
            .services
            .store
            .change_repo()
            .insert_changes(ctx.session.session.id, &changes)
            .await
        {
            Ok(change_ids) => change_ids,
            Err(error) => {
                restore_shell_change_set_baseline(
                    &ctx.services.edit_safety,
                    ctx.session.session.id,
                    &baseline_snapshot,
                )?;
                return Err(error.into());
            }
        };
        let change_summaries = changes
            .iter()
            .map(|change| crate::edit::ChangeSummary {
                change_id: change.id,
                kind: change.kind,
                path_before: change.path_before.clone(),
                path_after: change.path_after.clone(),
            })
            .collect::<Vec<_>>();
        let merged_output = format_shell_output_for_display(
            &input.command,
            &output.stdout,
            &output.stderr,
            output.exit_code,
            output.timed_out,
        );
        let preview = ctx.services.truncator.preview(
            merged_output,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: description,
            output_text: preview.preview_text,
            metadata: json!({
                "exit_code": output.exit_code,
                "timeout": output.timed_out,
                "truncated": preview.truncated,
                "changed_files": change_ids,
                "success": output.exit_code == Some(0) && !output.timed_out,
                "progress_effect": if output.exit_code == Some(0) && !output.timed_out { "made_progress" } else { "blocked" },
                "shell_output_projection": {
                    "command": input.command.clone(),
                    "stdout_present": !output.stdout.trim().is_empty(),
                    "stderr_present": !output.stderr.trim().is_empty(),
                    "exit_code": output.exit_code,
                    "timeout": output.timed_out,
                    "command_text_encoding_review": encoding_review.metadata(),
                    "retry_guidance": if output.exit_code == Some(0) && !output.timed_out {
                        "command_succeeded"
                    } else {
                        "review_stdout_stderr_and_retry_corrected_command_when_the_command_was_malformed"
                    }
                },
                "tool_feedback_envelope": {
                    "kind": "shell_execution_result",
                    "success": output.exit_code == Some(0) && !output.timed_out,
                    "progress_effect": if output.exit_code == Some(0) && !output.timed_out { "made_progress" } else { "blocked" },
                    "submitted_command": input.command.clone(),
                    "exit_code": output.exit_code,
                    "timeout": output.timed_out,
                    "stdout_present": !output.stdout.trim().is_empty(),
                    "stderr_present": !output.stderr.trim().is_empty(),
                    "command_text_encoding_review": encoding_review.metadata(),
                    "next_action_guidance": if output.exit_code == Some(0) && !output.timed_out {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String("If the command was malformed, inspect stdout/stderr, correct the command, and retry once with the native shell syntax. Do not stop solely because one shell command failed.".to_string())
                    },
                    "result_hash": crate::harness::artifact::hash_bytes(format!(
                        "shell|{}|{:?}|{}|{}",
                        input.command.clone(),
                        output.exit_code,
                        output.timed_out,
                        output.stderr
                    ).as_bytes())
                }
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: change_ids,
            change_summaries,
        })
    }
}

fn shell_contract_violation_result(command: &str, violation: ShellContractViolation) -> ToolResult {
    let result_hash = crate::harness::artifact::hash_bytes(
        format!(
            "shell_contract_violation|{}|{}|{}",
            violation.kind, command, violation.output_text
        )
        .as_bytes(),
    );
    let encoding_review = violation.encoding_review.clone();
    ToolResult {
        title: violation.title,
        output_text: violation.output_text,
        metadata: json!({
            "exit_code": null,
            "timeout": false,
            "truncated": false,
            "changed_files": [],
            "corrective_result": true,
            "contract_violation": violation.kind,
            "command_text_encoding_review": encoding_review,
            "success": false,
            "progress_effect": "no_progress",
            "result_hash": result_hash,
            "tool_feedback_envelope": {
                "kind": "shell_contract_violation",
                "success": false,
                "progress_effect": "no_progress",
                "tool": "shell",
                "submitted_command": command,
                "contract_violation": violation.kind,
                "command_text_encoding_review": violation.encoding_review,
                "side_effects_applied": false,
                "blocked_action": violation.kind,
                "required_next_action": "submit_corrected_native_shell_command",
                "result_hash": result_hash,
            },
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

struct ShellContractViolation {
    title: String,
    output_text: String,
    kind: &'static str,
    encoding_review: serde_json::Value,
}

fn shell_contract_violation(command: &str, family: ShellFamily) -> Option<ShellContractViolation> {
    let trimmed = command.trim();
    if matches!(family, ShellFamily::PowerShell) {
        if trimmed.contains("&&") {
            return Some(shell_syntax_violation(
                "This `shell` tool is running Windows PowerShell, and PowerShell 5.1 does not support `&&`. Rewrite the call using raw PowerShell syntax only. Prefer the `workdir` field instead of `cd ... && ...`, and if command chaining must depend on prior success, use `cmd1; if ($?) { cmd2 }`.",
            ));
        }
        if trimmed.contains("2>&1") {
            return Some(shell_syntax_violation(
                "Do not append `2>&1` when using this `shell` tool on Windows PowerShell. moyai already captures both stdout and stderr for you, and PowerShell 5.1 can turn native stderr redirection into `NativeCommandError` noise. Send the raw command directly without stderr redirection, using native shell syntax.",
            ));
        }
        if trimmed
            .to_ascii_lowercase()
            .starts_with("powershell -command")
        {
            return Some(shell_syntax_violation(
                "Do not wrap the command in `powershell -Command` when using this tool. Send the raw PowerShell command text directly, and use the `workdir` field to choose the directory.",
            ));
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("dir /") || lower.starts_with("dir\t/") {
            return Some(shell_syntax_violation(
                "This `shell` tool is running Windows PowerShell, and `dir /s /b` is CMD-style syntax. Do not use CMD switches here. Use targeted PowerShell syntax for a specific directory, or prefer `list`, `glob`, `grep`, and `read` for repository inspection instead of a broad shell relist.",
            ));
        }
        if starts_with_linux_diagnostic(trimmed)
            || lower.contains("| head")
            || lower.contains("| tail")
        {
            return Some(shell_syntax_violation(
                "This `shell` tool is running Windows PowerShell. Do not use Linux diagnostics such as `top`, `htop`, `free`, `uptime`, or pipes to `head` / `tail` here. Rewrite the command in native PowerShell. For read-only Windows system diagnostics requested by the user, prefer commands such as `Get-CimInstance Win32_Processor | Select-Object Name, LoadPercentage`, `Get-CimInstance Win32_OperatingSystem | Select-Object TotalVisibleMemorySize, FreePhysicalMemory`, `Get-Process`, or a short `Get-Process` CPU delta sample.",
            ));
        }
    }
    let encoding_review = command_text_encoding_review(command, family);
    if encoding_review.requires_correction {
        return Some(ShellContractViolation {
            title: "Clarify command text encoding".to_string(),
            output_text: encoding_review.feedback_text(command),
            kind: "command_text_encoding_contract",
            encoding_review: encoding_review.metadata(),
        });
    }
    None
}

fn shell_syntax_violation(output_text: &str) -> ShellContractViolation {
    ShellContractViolation {
        title: "Correct shell invocation".to_string(),
        output_text: output_text.to_string(),
        kind: "shell_syntax_contract",
        encoding_review: serde_json::Value::Null,
    }
}

fn format_shell_output_for_display(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    timed_out: bool,
) -> String {
    let mut sections = Vec::new();
    sections.push(format!("Command: {}", command.trim()));
    sections.push(format!(
        "Exit code: {}{}",
        exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        if timed_out { " (timeout)" } else { "" }
    ));
    sections.push(format!(
        "Stdout:\n{}",
        if stdout.trim().is_empty() {
            "(empty)"
        } else {
            stdout.trim_end()
        }
    ));
    sections.push(format!(
        "Stderr:\n{}",
        if stderr.trim().is_empty() {
            "(empty)"
        } else {
            stderr.trim_end()
        }
    ));
    if exit_code != Some(0) || timed_out {
        sections.push(
            "Recovery: inspect the stdout/stderr above. If the command was malformed, retry with a corrected native-shell command instead of stopping after this single failure."
                .to_string(),
        );
    }
    sections.join("\n\n")
}

fn starts_with_linux_diagnostic(command: &str) -> bool {
    let lower = command.trim().to_ascii_lowercase();
    [
        "top", "htop", "free", "uptime", "vmstat", "iostat", "mpstat", "sar",
    ]
    .into_iter()
    .any(|value| {
        lower.starts_with(value) && lower[value.len()..].starts_with([' ', '\t', '|', ';'])
    })
}

#[derive(Clone, Debug)]
struct CommandTextEncodingReview {
    status: &'static str,
    io_profile: &'static str,
    evidence: Vec<&'static str>,
    required_action: Option<&'static str>,
    suggested_command: Option<String>,
    requires_correction: bool,
}

impl CommandTextEncodingReview {
    fn metadata(&self) -> serde_json::Value {
        json!({
            "contract": "command_text_encoding_contract",
            "status": self.status,
            "io_profile": self.io_profile,
            "evidence": self.evidence,
            "required_action": self.required_action,
            "suggested_command": self.suggested_command,
            "requires_correction": self.requires_correction,
        })
    }

    fn feedback_text(&self, command: &str) -> String {
        let mut lines = vec![
            "Command text encoding contract review failed before execution.".to_string(),
            format!("Command: {}", command.trim()),
            format!("Status: {}", self.status),
            format!("I/O profile: {}", self.io_profile),
            "Reason: this command can execute code or tests that read, write, capture, or print text, but the submitted command relies on platform defaults or moyai's tool-owned UTF-8 bootstrap instead of an explicit command/artifact text-encoding contract.".to_string(),
        ];
        if let Some(action) = self.required_action {
            lines.push(format!("Required action: {action}"));
        }
        if let Some(suggestion) = &self.suggested_command {
            lines.push(format!("One acceptable corrected command: {suggestion}"));
        }
        lines.push(
            "Do not proceed with a text-producing verification command until the command or the generated artifact makes the text encoding contract explicit.".to_string(),
        );
        lines.join("\n")
    }
}

fn command_text_encoding_review(command: &str, family: ShellFamily) -> CommandTextEncodingReview {
    let trimmed = command.trim();
    let lower = trimmed.to_ascii_lowercase();
    let tokens = command_tokens(trimmed);
    let mut evidence = Vec::new();

    if !command_has_text_io_surface(&tokens, &lower) {
        return CommandTextEncodingReview {
            status: "not_text_io_command",
            io_profile: "no_known_text_io_surface",
            evidence,
            required_action: None,
            suggested_command: None,
            requires_correction: false,
        };
    }

    evidence.push("known_text_io_command_surface");
    if command_has_explicit_encoding_control(&lower) {
        evidence.push("explicit_encoding_control");
        return CommandTextEncodingReview {
            status: "encoding_explicit",
            io_profile: command_text_io_profile(&tokens, &lower),
            evidence,
            required_action: None,
            suggested_command: None,
            requires_correction: false,
        };
    }

    let inherited_from_tool_env = cfg!(windows)
        && matches!(family, ShellFamily::PowerShell)
        && command_inherits_tool_encoding_bootstrap(&tokens);
    if inherited_from_tool_env {
        evidence.push("moyai_shell_bootstrap_will_inject_utf8_environment");
    }

    let suggested_command = command_encoding_correction(trimmed, &tokens, &lower, family);
    let requires_correction = true;
    CommandTextEncodingReview {
        status: if inherited_from_tool_env {
            "encoding_inherited_from_tool_environment"
        } else {
            "encoding_unspecified"
        },
        io_profile: command_text_io_profile(&tokens, &lower),
        evidence,
        required_action: Some(
            "make the command or generated artifact text encoding explicit before execution",
        ),
        suggested_command,
        requires_correction,
    }
}

pub(crate) fn command_text_encoding_suggested_command(
    command: &str,
    family: ShellFamily,
) -> Option<String> {
    command_text_encoding_review(command, family).suggested_command
}

fn command_has_text_io_surface(tokens: &[String], lower: &str) -> bool {
    if lower.contains("encoding=")
        || lower.contains("stdout")
        || lower.contains("stderr")
        || lower.contains("read-host")
        || lower.contains("write-host")
        || lower.contains("write-output")
        || lower.contains("set-content")
        || lower.contains("get-content")
        || lower.contains("out-file")
        || lower.contains("tee-object")
    {
        return true;
    }
    language_command_text_io_surface_evidence(tokens, lower)
        || tokens
            .iter()
            .any(|token| matches!(token.as_str(), "powershell" | "pwsh"))
}

fn command_text_io_profile(tokens: &[String], lower: &str) -> &'static str {
    if tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "get-content" | "set-content" | "out-file" | "tee-object" | "write-output"
        )
    }) {
        return "text_producing_or_text_consuming_command";
    }
    if language_command_test_or_verification_io_evidence(tokens, lower) {
        "test_or_verification_runner"
    } else if language_runtime_execution_io_evidence(tokens) {
        "language_runtime_execution"
    } else {
        "text_producing_or_text_consuming_command"
    }
}

fn command_has_explicit_encoding_control(lower: &str) -> bool {
    [
        "utf-8",
        "utf8",
        "pythonutf8",
        "pythonioencoding",
        "-x utf8",
        "outputencoding",
        "console]::inputencoding",
        "console]::outputencoding",
        "chcp 65001",
        "encoding=",
        "charset",
        "lang=c.utf8",
        "lang=c.utf-8",
        "lc_all=c.utf8",
        "lc_all=c.utf-8",
    ]
    .into_iter()
    .any(|needle| lower.contains(needle))
}

fn command_inherits_tool_encoding_bootstrap(tokens: &[String]) -> bool {
    language_command_inherits_utf8_bootstrap(tokens)
}

fn command_encoding_correction(
    command: &str,
    tokens: &[String],
    lower: &str,
    family: ShellFamily,
) -> Option<String> {
    if language_python_utf8_correction_applies(tokens) {
        return Some(correct_python_command_for_utf8(command, lower, family));
    }
    if tokens
        .iter()
        .any(|token| matches!(token.as_str(), "powershell" | "pwsh"))
    {
        return Some(format!(
            "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new(); [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); {command}"
        ));
    }
    Some(match family {
        ShellFamily::PowerShell => format!(
            "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new(); [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); $env:LANG='C.UTF-8'; $env:LC_ALL='C.UTF-8'; {command}"
        ),
        ShellFamily::Bash => format!("LC_ALL=C.UTF-8 LANG=C.UTF-8 {command}"),
    })
}

fn correct_python_command_for_utf8(command: &str, lower: &str, family: ShellFamily) -> String {
    if lower.starts_with("python ") {
        return format!("python -X utf8 {}", command["python ".len()..].trim_start());
    }
    if lower.starts_with("python3 ") {
        return format!(
            "python3 -X utf8 {}",
            command["python3 ".len()..].trim_start()
        );
    }
    if lower.starts_with("py ") {
        return format!("py -X utf8 {}", command["py ".len()..].trim_start());
    }
    if lower.starts_with("pytest") {
        return match family {
            ShellFamily::PowerShell => {
                format!("$env:PYTHONUTF8='1'; $env:PYTHONIOENCODING='utf-8'; {command}")
            }
            ShellFamily::Bash => format!("PYTHONUTF8=1 PYTHONIOENCODING=utf-8 {command}"),
        };
    }
    match family {
        ShellFamily::PowerShell => {
            format!("$env:PYTHONUTF8='1'; $env:PYTHONIOENCODING='utf-8'; {command}")
        }
        ShellFamily::Bash => format!("PYTHONUTF8=1 PYTHONIOENCODING=utf-8 {command}"),
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timed_out: bool,
}

async fn execute_shell_command(
    shell: &crate::config::ShellConfig,
    workdir: &Utf8Path,
    command_text: &str,
    timeout_ms: u64,
    cancel: CancellationToken,
) -> Result<CommandOutput, ToolError> {
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
    configure_process_group(&mut command);
    command.kill_on_drop(true);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| ToolError::Message("spawned shell process has no process id".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::Message("stdout pipe was not captured".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::Message("stderr pipe was not captured".to_string()))?;
    let stdout_task = tokio::spawn(async move { read_pipe(stdout).await });
    let stderr_task = tokio::spawn(async move { read_pipe(stderr).await });

    let (status, timed_out) = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = terminate_shell_child(&mut child, pid).await;
            return Err(ToolError::Message("shell command cancelled by user".to_string()));
        }
        result = timeout(Duration::from_millis(timeout_ms), child.wait()) => match result {
            Ok(result) => (result?, false),
            Err(_) => {
                let status = terminate_shell_child(&mut child, pid).await?;
                (status, true)
            }
        }
    };

    cleanup_shell_process_tree_after_parent_exit(pid).await?;

    let stdout = decode_shell_bytes_for_display(&join_pipe(stdout_task, "stdout").await?);
    let stderr_bytes = join_pipe(stderr_task, "stderr").await?;
    let stderr = if timed_out {
        if stderr_bytes.is_empty() {
            "command timed out".to_string()
        } else {
            format!(
                "{}\ncommand timed out",
                decode_shell_bytes_for_display(&stderr_bytes)
            )
        }
    } else {
        decode_shell_bytes_for_display(&stderr_bytes)
    };

    Ok(CommandOutput {
        stdout,
        stderr,
        exit_code: status.code(),
        timed_out,
    })
}

async fn read_pipe<T>(mut pipe: T) -> Result<Vec<u8>, std::io::Error>
where
    T: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn join_pipe(
    task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    label: &str,
) -> Result<Vec<u8>, ToolError> {
    task.await
        .map_err(|error| {
            ToolError::Message(format!("failed to join shell {label} reader task: {error}"))
        })?
        .map_err(|error| ToolError::Message(format!("failed to read shell {label}: {error}")))
}

fn apply_shell_environment(command: &mut Command, shell: &crate::config::ShellConfig) {
    let mut captured = HashMap::new();
    for key in &shell.env_allowlist {
        if let Some(value) = std::env::var_os(key) {
            captured.insert(key.clone(), value);
        }
    }
    let injected = platform_bootstrap_env(&captured);

    #[cfg(windows)]
    {
        for key in &shell.env_allowlist {
            if let Some(value) = captured.get(key) {
                command.env(key, value);
            }
        }
        for (key, value) in injected {
            command.env(key, value);
        }
    }

    #[cfg(not(windows))]
    command.env_clear();
    #[cfg(not(windows))]
    {
        for key in &shell.env_allowlist {
            if let Some(value) = captured.get(key) {
                command.env(key, value);
            }
        }
        for (key, value) in injected {
            command.env(key, value);
        }
    }
}

fn platform_bootstrap_env(
    captured: &HashMap<String, std::ffi::OsString>,
) -> Vec<(String, std::ffi::OsString)> {
    #[cfg(windows)]
    {
        let _ = captured;
        vec![
            ("PYTHONUTF8".to_string(), std::ffi::OsString::from("1")),
            (
                "PYTHONIOENCODING".to_string(),
                std::ffi::OsString::from("utf-8"),
            ),
        ]
    }
    #[cfg(not(windows))]
    {
        let _ = captured;
        Vec::new()
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
async fn kill_process_tree(pid: u32) -> Result<(), ToolError> {
    let process_group = format!("-{pid}");
    let _ = Command::new("kill")
        .args(["-TERM", &process_group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = Command::new("kill")
        .args(["-KILL", &process_group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    Ok(())
}

#[cfg(windows)]
async fn kill_process_tree(pid: u32) -> Result<(), ToolError> {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    kill_process_descendants_by_parent_id(pid).await?;
    Ok(())
}

#[cfg(windows)]
async fn cleanup_shell_process_tree_after_parent_exit(pid: u32) -> Result<(), ToolError> {
    kill_process_descendants_by_parent_id(pid).await
}

#[cfg(windows)]
async fn kill_process_descendants_by_parent_id(pid: u32) -> Result<(), ToolError> {
    let script = format!(
        r#"
$root = {pid}
$processes = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
$known = @{{}}
$known[$root] = $true
$descendants = New-Object System.Collections.Generic.List[int]
do {{
    $found = $false
    foreach ($process in $processes) {{
        $processId = [int]$process.ProcessId
        $parentId = [int]$process.ParentProcessId
        if ($known.ContainsKey($parentId) -and -not $known.ContainsKey($processId)) {{
            $known[$processId] = $true
            $descendants.Add($processId)
            $found = $true
        }}
    }}
}} while ($found)
foreach ($processId in ($descendants | Sort-Object -Descending)) {{
    try {{ Stop-Process -Id $processId -Force -ErrorAction Stop }} catch {{ }}
}}
"#
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    Ok(())
}

#[cfg(unix)]
async fn cleanup_shell_process_tree_after_parent_exit(pid: u32) -> Result<(), ToolError> {
    kill_process_tree(pid).await
}

#[cfg(not(any(unix, windows)))]
async fn cleanup_shell_process_tree_after_parent_exit(_pid: u32) -> Result<(), ToolError> {
    Ok(())
}

async fn terminate_shell_child(
    child: &mut tokio::process::Child,
    pid: u32,
) -> Result<ExitStatus, ToolError> {
    for step in shell_timeout_termination_plan() {
        match step {
            ShellTerminationStep::ProcessTreeKill => {
                kill_process_tree(pid).await?;
            }
            ShellTerminationStep::ParentStartKill => {
                let _ = child.start_kill();
            }
            ShellTerminationStep::WaitForParent => {
                return timeout(Duration::from_secs(5), child.wait())
                    .await
                    .map_err(|_| {
                        ToolError::Message(
                            "shell command timed out and could not be terminated cleanly"
                                .to_string(),
                        )
                    })?
                    .map_err(ToolError::from);
            }
        }
    }
    Err(ToolError::Message(
        "shell command timed out and no termination wait step was configured".to_string(),
    ))
}

#[cfg(not(any(unix, windows)))]
async fn kill_process_tree(_pid: u32) -> Result<(), ToolError> {
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellTerminationStep {
    ParentStartKill,
    ProcessTreeKill,
    WaitForParent,
}

fn shell_timeout_termination_plan() -> Vec<ShellTerminationStep> {
    vec![
        ShellTerminationStep::ProcessTreeKill,
        ShellTerminationStep::ParentStartKill,
        ShellTerminationStep::WaitForParent,
    ]
}

pub(crate) fn shell_timeout_process_tree_termination_order_fixture_passes() -> bool {
    let plan = shell_timeout_termination_plan();
    let process_tree_position = plan
        .iter()
        .position(|step| *step == ShellTerminationStep::ProcessTreeKill);
    let parent_position = plan
        .iter()
        .position(|step| *step == ShellTerminationStep::ParentStartKill);

    matches!(
        (process_tree_position, parent_position),
        (Some(process_tree), Some(parent)) if process_tree < parent
    ) && plan.last() == Some(&ShellTerminationStep::WaitForParent)
}

pub(crate) fn shell_contract_violation_typed_no_progress_feedback_fixture_passes() -> bool {
    let Some(syntax_violation) = shell_contract_violation("dir /s /b", ShellFamily::PowerShell)
    else {
        return false;
    };
    let syntax_result = shell_contract_violation_result("dir /s /b", syntax_violation);
    if !shell_contract_violation_result_has_typed_no_progress_feedback(
        &syntax_result,
        "shell_syntax_contract",
        "dir /s /b",
    ) {
        return false;
    }

    let command = "python -m workflow_check";
    let Some(encoding_violation) = shell_contract_violation(command, ShellFamily::PowerShell)
    else {
        return false;
    };
    let encoding_result = shell_contract_violation_result(command, encoding_violation);
    shell_contract_violation_result_has_typed_no_progress_feedback(
        &encoding_result,
        "command_text_encoding_contract",
        command,
    ) && encoding_result
        .metadata
        .pointer("/tool_feedback_envelope/command_text_encoding_review/contract")
        .and_then(serde_json::Value::as_str)
        == Some("command_text_encoding_contract")
}

fn shell_contract_violation_result_has_typed_no_progress_feedback(
    result: &ToolResult,
    violation_kind: &str,
    command: &str,
) -> bool {
    result.recorded_changes.is_empty()
        && result.change_summaries.is_empty()
        && result
            .metadata
            .get("success")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && result
            .metadata
            .get("progress_effect")
            .and_then(serde_json::Value::as_str)
            == Some("no_progress")
        && result
            .metadata
            .get("contract_violation")
            .and_then(serde_json::Value::as_str)
            == Some(violation_kind)
        && result
            .metadata
            .get("result_hash")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty())
        && result
            .metadata
            .pointer("/tool_feedback_envelope/success")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && result
            .metadata
            .pointer("/tool_feedback_envelope/progress_effect")
            .and_then(serde_json::Value::as_str)
            == Some("no_progress")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && result
            .metadata
            .pointer("/tool_feedback_envelope/submitted_command")
            .and_then(serde_json::Value::as_str)
            == Some(command)
        && result
            .metadata
            .pointer("/tool_feedback_envelope/contract_violation")
            .and_then(serde_json::Value::as_str)
            == Some(violation_kind)
        && result
            .metadata
            .pointer("/tool_feedback_envelope/required_next_action")
            .and_then(serde_json::Value::as_str)
            == Some("submit_corrected_native_shell_command")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/result_hash")
            .and_then(serde_json::Value::as_str)
            == result
                .metadata
                .get("result_hash")
                .and_then(serde_json::Value::as_str)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellCompletionCleanupStep {
    DescendantProcessTreeCleanup,
    JoinCapturedPipes,
}

fn shell_completion_cleanup_plan() -> Vec<ShellCompletionCleanupStep> {
    vec![
        ShellCompletionCleanupStep::DescendantProcessTreeCleanup,
        ShellCompletionCleanupStep::JoinCapturedPipes,
    ]
}

pub(crate) fn shell_completion_process_tree_cleanup_fixture_passes() -> bool {
    let plan = shell_completion_cleanup_plan();
    let cleanup_position = plan
        .iter()
        .position(|step| *step == ShellCompletionCleanupStep::DescendantProcessTreeCleanup);
    let pipe_join_position = plan
        .iter()
        .position(|step| *step == ShellCompletionCleanupStep::JoinCapturedPipes);
    matches!(
        (cleanup_position, pipe_join_position),
        (Some(cleanup), Some(pipe_join)) if cleanup < pipe_join
    )
}

fn references_outside_workspace(workspace: &crate::workspace::Workspace, command: &str) -> bool {
    if command.contains("..") {
        return true;
    }

    extract_absolute_paths(command).into_iter().any(|path| {
        !path.starts_with(&workspace.root)
            && !workspace
                .path_policy
                .additional_write_roots
                .iter()
                .any(|root| path.starts_with(root))
    })
}

fn extract_absolute_paths(command: &str) -> Vec<Utf8PathBuf> {
    let mut paths = Vec::new();

    if cfg!(windows) {
        let regex = Regex::new(r#"(?i)[A-Z]:[\\/][^\s"'|;]+"#).expect("windows path regex");
        for value in regex.find_iter(command).map(|capture| capture.as_str()) {
            let normalized = value.replace('/', "\\");
            let path = Utf8PathBuf::from(normalized);
            if path.is_absolute() {
                paths.push(path);
            }
        }
    } else {
        let regex = Regex::new(r#"/[^\s"'|;]+"#).expect("unix path regex");
        for value in regex.find_iter(command).map(|capture| capture.as_str()) {
            let path = Utf8PathBuf::from(value);
            if path.is_absolute() {
                paths.push(path);
            }
        }
    }

    paths
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
        format!("Workdir: {}", workdir),
    ]
}

fn shell_permission_risks(
    workspace: &crate::workspace::Workspace,
    command: &str,
) -> Vec<PermissionRisk> {
    let mut risks = Vec::new();
    if shell_has_delete_risk(command) {
        risks.push(PermissionRisk::DestructiveDelete);
    }
    if shell_has_move_risk(command) {
        risks.push(PermissionRisk::MoveOrRename);
    }
    if shell_has_network_risk(command) {
        risks.push(PermissionRisk::Network);
    }
    if shell_requires_external_connection_review(command) {
        risks.push(PermissionRisk::ExternalConnection);
    }
    if command_mentions_protected_target(workspace, command) {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    risks
}

fn shell_has_delete_risk(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "remove-item",
        "rm ",
        "del ",
        "erase ",
        "rmdir ",
        "rd ",
        "remove-item ",
    ]
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
        return tokens.iter().any(|token| matches!(token.as_str(), "npx"))
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
    extract_absolute_paths(command)
        .into_iter()
        .any(|path| is_protected_workspace_authority_path(&workspace.root, &path))
}

#[derive(Clone)]
struct SnapshotEntry {
    bytes: Option<Vec<u8>>,
    text: Option<String>,
}

fn snapshot_workspace(
    workspace: &crate::workspace::Workspace,
) -> Result<HashMap<Utf8PathBuf, SnapshotEntry>, ToolError> {
    let ignore = workspace.ignore.compile()?;
    let mut builder = WalkBuilder::new(&workspace.root);
    builder.hidden(false);
    builder.git_ignore(workspace.ignore.use_gitignore);
    let mut snapshot = HashMap::new();

    for entry in builder.build() {
        let entry = entry.map_err(|error| ToolError::Message(error.to_string()))?;
        if !entry
            .file_type()
            .map(|value| value.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let path = Utf8PathBuf::from_path_buf(entry.path().to_path_buf())
            .map_err(|_| ToolError::Message("path is not valid UTF-8".to_string()))?;
        if workspace
            .protected_paths
            .iter()
            .any(|value| path.starts_with(value))
        {
            continue;
        }
        if workspace
            .ignore
            .matches_compiled(&ignore, &workspace.root, &path)
        {
            continue;
        }
        let bytes = fs::read(&path)?;
        let text = String::from_utf8(bytes.clone()).ok();
        snapshot.insert(
            path,
            SnapshotEntry {
                bytes: Some(bytes),
                text,
            },
        );
    }

    Ok(snapshot)
}

struct ShellChangeSet {
    changes: Vec<crate::edit::FileChange>,
    removed_paths: Vec<Utf8PathBuf>,
    current_paths: Vec<Utf8PathBuf>,
}

impl ShellChangeSet {
    fn baseline_paths(&self) -> Vec<Utf8PathBuf> {
        let mut paths = self
            .removed_paths
            .iter()
            .chain(self.current_paths.iter())
            .cloned()
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }
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

fn sync_shell_change_set(
    edit_safety: &crate::edit::EditSafety,
    session_id: crate::session::SessionId,
    shell_changes: &ShellChangeSet,
) -> Result<(), ToolError> {
    edit_safety
        .sync_file_mutations(
            session_id,
            &shell_changes.removed_paths,
            &shell_changes.current_paths,
        )
        .map_err(ToolError::from)
}

fn snapshot_and_sync_shell_change_set(
    edit_safety: &crate::edit::EditSafety,
    session_id: crate::session::SessionId,
    shell_changes: &ShellChangeSet,
) -> Result<Vec<(Utf8PathBuf, Option<crate::edit::FileReadStamp>)>, ToolError> {
    let baseline_snapshot =
        edit_safety.snapshot_path_stamps(session_id, &shell_changes.baseline_paths());
    sync_shell_change_set(edit_safety, session_id, shell_changes)?;
    Ok(baseline_snapshot)
}

fn restore_shell_change_set_baseline(
    edit_safety: &crate::edit::EditSafety,
    session_id: crate::session::SessionId,
    baseline_snapshot: &[(Utf8PathBuf, Option<crate::edit::FileReadStamp>)],
) -> Result<(), ToolError> {
    edit_safety
        .restore_path_stamps(session_id, baseline_snapshot)
        .map_err(ToolError::from)
}

fn build_shell_changes(
    ctx: &ToolContext<'_>,
    before: HashMap<Utf8PathBuf, SnapshotEntry>,
    after: HashMap<Utf8PathBuf, SnapshotEntry>,
) -> Result<ShellChangeSet, ToolError> {
    let mut all_paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    all_paths.sort();
    all_paths.dedup();

    let mut changes = Vec::new();
    let mut removed_paths = Vec::new();
    let mut current_paths = Vec::new();
    for path in all_paths {
        let before_entry = before.get(&path);
        let after_entry = after.get(&path);
        let changed = match (before_entry, after_entry) {
            (Some(before), Some(after)) => before.bytes != after.bytes,
            (None, Some(_)) | (Some(_), None) => true,
            (None, None) => false,
        };
        if !changed {
            continue;
        }
        if before_entry.is_some() {
            removed_paths.push(path.clone());
        }
        if after_entry.is_some() {
            current_paths.push(path.clone());
        }
        let before_text = before_entry
            .and_then(|entry| entry.text.clone())
            .unwrap_or_else(|| "<<binary>>".to_string());
        let after_text = after_entry
            .and_then(|entry| entry.text.clone())
            .unwrap_or_else(|| "<<binary>>".to_string());
        let change = ctx.services.change_tracker.build_change(
            ctx.tool_call_id,
            before_entry
                .as_ref()
                .map(|_| path_for_change_storage(path.as_path(), &ctx.workspace.root))
                .as_deref(),
            after_entry
                .as_ref()
                .map(|_| path_for_change_storage(path.as_path(), &ctx.workspace.root))
                .as_deref(),
            before_entry.as_ref().map(|_| before_text.as_str()),
            after_entry.as_ref().map(|_| after_text.as_str()),
        )?;
        changes.push(change);
    }
    Ok(ShellChangeSet {
        changes,
        removed_paths,
        current_paths,
    })
}

pub(crate) fn shell_change_set_syncs_confirmed_edit_baseline_fixture_passes() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let path = match Utf8PathBuf::from_path_buf(temp.path().join("shell-edited.txt")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if fs::write(&path, "before").is_err() {
        return false;
    }
    let edit_safety = crate::edit::EditSafety::default();
    let session_id = crate::session::SessionId::new();
    if edit_safety
        .record_current_file_state(session_id, &path)
        .is_err()
    {
        return false;
    }
    if fs::write(&path, "after").is_err() {
        return false;
    }
    let shell_changes = ShellChangeSet {
        changes: Vec::new(),
        removed_paths: vec![path.clone()],
        current_paths: vec![path.clone()],
    };
    if sync_shell_change_set(&edit_safety, session_id, &shell_changes).is_err() {
        return false;
    }
    let metadata = match fs::metadata(&path) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64);
    edit_safety
        .assert_fresh_write(session_id, &path, mtime_ms, Some(metadata.len()))
        .is_ok()
}

pub(crate) fn shell_change_set_restores_baseline_on_persistence_failure_fixture_passes() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let path = match Utf8PathBuf::from_path_buf(temp.path().join("shell-edited.txt")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if fs::write(&path, "before").is_err() {
        return false;
    }
    let edit_safety = crate::edit::EditSafety::default();
    let session_id = crate::session::SessionId::new();
    if edit_safety
        .record_current_file_state(session_id, &path)
        .is_err()
    {
        return false;
    }
    let before_stamp = edit_safety.get_stamp(session_id, &path);
    if before_stamp.is_none() || fs::write(&path, "after").is_err() {
        return false;
    }
    let shell_changes = ShellChangeSet {
        changes: Vec::new(),
        removed_paths: vec![path.clone()],
        current_paths: vec![path.clone()],
    };
    let baseline_snapshot =
        match snapshot_and_sync_shell_change_set(&edit_safety, session_id, &shell_changes) {
            Ok(value) => value,
            Err(_) => return false,
        };
    if edit_safety.get_stamp(session_id, &path) == before_stamp {
        return false;
    }
    restore_shell_change_set_baseline(&edit_safety, session_id, &baseline_snapshot).is_ok()
        && edit_safety.get_stamp(session_id, &path) == before_stamp
}

pub(crate) fn shell_output_encoding_fixture_passes() -> bool {
    let cp932_japanese = [
        0x8e, 0xa9, 0x91, 0x52, 0x91, 0xce, 0x90, 0x94, 0x82, 0xcc, 0x8a, 0xee, 0x96, 0x7b, 0x93,
        0x49, 0x82, 0xc8, 0x92, 0x6c,
    ];
    if decode_shell_bytes_for_display(&cp932_japanese) != "自然対数の基本的な値" {
        return false;
    }
    if decode_shell_bytes_for_display("自然対数の基本的な値".as_bytes()) != "自然対数の基本的な値"
    {
        return false;
    }
    let env = platform_bootstrap_env(&HashMap::new());
    let env_map = env
        .into_iter()
        .map(|(key, value)| (key, value.to_string_lossy().into_owned()))
        .collect::<HashMap<_, _>>();
    if cfg!(windows) {
        env_map.get("PYTHONUTF8").map(String::as_str) == Some("1")
            && env_map.get("PYTHONIOENCODING").map(String::as_str) == Some("utf-8")
    } else {
        true
    }
}

pub fn command_text_encoding_contract_fixture_passes() -> bool {
    let python_plain =
        command_text_encoding_review("python -m workflow_check", ShellFamily::PowerShell);
    if python_plain.status != "encoding_inherited_from_tool_environment"
        || !python_plain.requires_correction
        || python_plain.suggested_command.as_deref() != Some("python -X utf8 -m workflow_check")
    {
        return false;
    }

    let python_explicit =
        command_text_encoding_review("python -X utf8 -m workflow_check", ShellFamily::PowerShell);
    if python_explicit.status != "encoding_explicit" || python_explicit.requires_correction {
        return false;
    }

    let node_plain = command_text_encoding_review("node test.js", ShellFamily::PowerShell);
    if !matches!(
        node_plain.io_profile,
        "language_runtime_execution" | "test_or_verification_runner"
    ) || !node_plain.requires_correction
        || node_plain.status != "encoding_unspecified"
        || node_plain.suggested_command.as_deref()
            != Some(
                "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new(); [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); $env:LANG='C.UTF-8'; $env:LC_ALL='C.UTF-8'; node test.js",
            )
    {
        return false;
    }

    let node_builtin_test = command_text_encoding_review("node --test", ShellFamily::PowerShell);
    if node_builtin_test.io_profile != "test_or_verification_runner"
        || node_builtin_test.status != "encoding_unspecified"
        || !node_builtin_test.requires_correction
    {
        return false;
    }

    let bun_test = command_text_encoding_review("bun test", ShellFamily::PowerShell);
    if bun_test.io_profile != "test_or_verification_runner"
        || bun_test.status != "encoding_unspecified"
        || !bun_test.requires_correction
    {
        return false;
    }

    let deno_test = command_text_encoding_review("deno test", ShellFamily::PowerShell);
    if deno_test.io_profile != "test_or_verification_runner"
        || deno_test.status != "encoding_unspecified"
        || !deno_test.requires_correction
    {
        return false;
    }

    let diagnostic = command_text_encoding_review(
        "Get-Process | Select-Object -First 5",
        ShellFamily::PowerShell,
    );
    if diagnostic.status != "not_text_io_command" || diagnostic.requires_correction {
        return false;
    }

    let get_content_utf8 = command_text_encoding_review(
        "Get-Content src/workflow.rs -Encoding UTF8",
        ShellFamily::PowerShell,
    );
    if get_content_utf8.status != "encoding_explicit"
        || get_content_utf8.requires_correction
        || get_content_utf8.io_profile != "text_producing_or_text_consuming_command"
    {
        return false;
    }

    let metadata = python_plain.metadata();
    metadata.get("contract").and_then(|value| value.as_str())
        == Some("command_text_encoding_contract")
        && metadata.get("status").and_then(|value| value.as_str())
            == Some("encoding_inherited_from_tool_environment")
}

pub fn external_connection_shell_review_fixture_passes() -> bool {
    let reviewed = [
        "pip install requests",
        "python -m pip install rich",
        "uv add pytest",
        "uv run pytest",
        "npm install",
        "pnpm add vite",
        "cargo fetch",
        "Invoke-WebRequest https://www.python.org/ftp/python.exe",
        "curl https://example.com/script.ps1",
        "git clone https://example.com/repo.git",
    ];
    reviewed
        .iter()
        .all(|command| shell_requires_external_connection_review(command))
        && shell_permission_details(
            "pip install pygame",
            Utf8Path::new("C:/Users/example/project"),
        )
        .iter()
        .any(|detail| detail == "Command: pip install pygame")
        && !shell_requires_external_connection_review("python -m unittest")
        && !shell_requires_external_connection_review("Get-Process | Select-Object -First 5")
}

pub fn shell_output_projection_fixture_passes() -> bool {
    let output =
        format_shell_output_for_display("Get-Process", "powershell  123", "", Some(0), false);
    let failed = format_shell_output_for_display("uv add ということ", "", "error", Some(1), false);
    output.contains("Command: Get-Process")
        && output.contains("Stdout:\npowershell  123")
        && output.contains("Stderr:\n(empty)")
        && failed.contains("Exit code: 1")
        && failed.contains("Recovery:")
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_change_set_syncs_confirmed_edit_baseline() {
        assert!(super::shell_change_set_syncs_confirmed_edit_baseline_fixture_passes());
    }

    #[test]
    fn shell_change_set_restores_baseline_on_persistence_failure() {
        assert!(super::shell_change_set_restores_baseline_on_persistence_failure_fixture_passes());
    }

    #[test]
    fn shell_output_encoding_preserves_japanese_text() {
        assert!(super::shell_output_encoding_fixture_passes());
    }

    #[test]
    fn command_text_encoding_contract_reviews_text_io_commands() {
        assert!(super::command_text_encoding_contract_fixture_passes());
    }

    #[test]
    fn external_connection_shell_commands_require_review() {
        assert!(super::external_connection_shell_review_fixture_passes());
    }

    #[test]
    fn shell_output_projection_includes_stdout_stderr_and_recovery() {
        assert!(super::shell_output_projection_fixture_passes());
    }
}
