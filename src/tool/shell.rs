use std::collections::HashMap;
use std::fs;
use std::io::Read as _;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use encoding_rs::SHIFT_JIS;
use ignore::WalkBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

use crate::config::ShellFamily;
use crate::edit::path_for_change_storage;
use crate::error::ToolError;
use crate::session::ChangeRepository;
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

const DEFAULT_SHELL_SNAPSHOT_LIMITS: SnapshotLimits = SnapshotLimits {
    max_walk_entries: 20_000,
    max_files: 10_000,
    max_total_bytes: 64 * 1024 * 1024,
    max_file_bytes: 8 * 1024 * 1024,
};

#[async_trait(?Send)]
impl Tool for ShellTool {
    fn spec(&self) -> ToolSpec {
        let description = if cfg!(windows) {
            "Run a shell command with the current user account. On Windows this tool executes PowerShell, so send raw PowerShell syntax only. Permission review classifies detected literal targets and risks; it is not an OS filesystem sandbox."
        } else {
            "Run a shell command with the current user account. On Unix this tool executes bash. Permission review classifies detected literal targets and risks; it is not an OS filesystem sandbox."
        };
        ToolSpec {
            name: ToolName::Shell,
            effect: crate::tool::ToolEffectPolicy::destructive(),
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
                        "description": "Optional working directory, preferably relative to the workspace root. Use the narrowest directory that contains the command targets; shell change snapshots are bounded to this scope and explicit absolute targets."
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
            || references_outside_workspace_from(ctx.workspace, &guarded.absolute, &input.command);
        let description = if input.description.trim().is_empty() {
            default_description(&input.command)
        } else {
            input.description.clone()
        };
        let encoding_review = command_text_encoding_review(&input.command, family);
        let risks = shell_permission_risks_from(ctx.workspace, &guarded.absolute, &input.command);
        let effect_admission = ctx
            .confirm_if_needed_with_details(
                AccessKind::Shell,
                description.clone(),
                shell_permission_details(&input.command, guarded.absolute.as_path()),
                vec![guarded.absolute.clone()],
                outside_workspace,
                risks,
            )
            .await?;

        let timeout_ms = input
            .timeout_ms
            .unwrap_or(ctx.config.shell.default_timeout_ms)
            .min(ctx.config.shell.max_timeout_ms);
        let snapshot_plan = shell_snapshot_plan(ctx.workspace, &guarded, &input.command);
        let before = match snapshot_workspace(
            ctx.workspace,
            &snapshot_plan,
            DEFAULT_SHELL_SNAPSHOT_LIMITS,
        ) {
            Ok(snapshot) => snapshot,
            Err(ShellSnapshotError::Limit(message)) => {
                return shell_snapshot_limit_result(
                    &ctx,
                    &description,
                    &input.command,
                    &snapshot_plan,
                    DEFAULT_SHELL_SNAPSHOT_LIMITS,
                    &message,
                    SnapshotLimitPhase::BeforeExecution,
                    None,
                );
            }
            Err(ShellSnapshotError::Other(error)) => return Err(error),
        };
        ctx.run_mutation_fence.assert_owned().await?;
        effect_admission.admit()?;
        let output = execute_shell_command(
            &ctx.config.shell,
            &guarded.absolute,
            &input.command,
            timeout_ms,
            ctx.config.tool_output.max_bytes.max(1),
            ctx.cancel.clone(),
        )
        .await?;
        let after = match snapshot_workspace(
            ctx.workspace,
            &snapshot_plan,
            DEFAULT_SHELL_SNAPSHOT_LIMITS,
        ) {
            Ok(snapshot) => snapshot,
            Err(ShellSnapshotError::Limit(message)) => {
                ctx.services
                    .edit_safety
                    .invalidate_roots(ctx.session.session.id, &snapshot_plan.scopes)?;
                return shell_snapshot_limit_result(
                    &ctx,
                    &description,
                    &input.command,
                    &snapshot_plan,
                    DEFAULT_SHELL_SNAPSHOT_LIMITS,
                    &message,
                    SnapshotLimitPhase::AfterExecution,
                    Some(&output),
                );
            }
            Err(ShellSnapshotError::Other(error)) => return Err(error),
        };
        let shell_changes = build_shell_changes(&ctx, before, after)?;
        let baseline_snapshot = snapshot_and_sync_shell_change_set(
            &ctx.services.edit_safety,
            ctx.session.session.id,
            &shell_changes,
        )?;
        let changes = shell_changes.changes;
        // A cancelled process can already have changed files. The per-session process lock remains
        // held while this tool finishes, so record the observed evidence even after cancellation;
        // an active-only mutation fence here would discard the audit trail for real side effects.
        let change_ids = match ctx
            .services
            .store
            .change_repo()
            .insert_changes(&changes)
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
            output.cancelled,
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
                "cancelled": output.cancelled,
                "stdout_capture_truncated": output.stdout_truncated,
                "stderr_capture_truncated": output.stderr_truncated,
                "snapshot_owner": snapshot_plan.owner_root,
                "snapshot_scopes": snapshot_plan.scopes,
                "snapshot_limits": DEFAULT_SHELL_SNAPSHOT_LIMITS.metadata(),
                "truncated": preview.truncated,
                "changed_files": change_ids,
                "success": output.exit_code == Some(0) && !output.timed_out && !output.cancelled,
                "shell_output_projection": {
                    "command": input.command.clone(),
                    "stdout_present": !output.stdout.trim().is_empty(),
                    "stderr_present": !output.stderr.trim().is_empty(),
                    "exit_code": output.exit_code,
                    "timeout": output.timed_out,
                    "cancelled": output.cancelled,
                    "stdout_capture_truncated": output.stdout_truncated,
                    "stderr_capture_truncated": output.stderr_truncated,
                    "command_text_encoding_review": encoding_review.metadata()
                }
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: change_ids,
            change_summaries,
        })
    }
}

fn shell_contract_violation_result(command: &str, violation: ShellContractViolation) -> ToolResult {
    let encoding_review = violation.encoding_review.clone();
    ToolResult {
        title: violation.title,
        output_text: violation.output_text,
        metadata: json!({
            "exit_code": null,
            "timeout": false,
            "truncated": false,
            "changed_files": [],
            "submitted_command": command,
            "contract_violation": violation.kind,
            "command_text_encoding_review": encoding_review,
            "success": false,
            "side_effects_applied": false
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
    cancelled: bool,
) -> String {
    let mut sections = Vec::new();
    sections.push(format!("Command: {}", command.trim()));
    sections.push(format!(
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
    if exit_code != Some(0) || timed_out || cancelled {
        sections.push(
            "Recovery: inspect the stdout/stderr above. If the command was malformed, retry with a corrected native-shell command instead of stopping after this single failure."
                .to_string(),
        );
    }
    sections.join("\n\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotLimitPhase {
    BeforeExecution,
    AfterExecution,
}

impl SnapshotLimitPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::BeforeExecution => "before_execution",
            Self::AfterExecution => "after_execution",
        }
    }

    fn side_effects_applied(self) -> bool {
        matches!(self, Self::AfterExecution)
    }
}

fn shell_snapshot_limit_result(
    ctx: &ToolContext<'_>,
    description: &str,
    command: &str,
    plan: &ShellSnapshotPlan,
    limits: SnapshotLimits,
    diagnostic: &str,
    phase: SnapshotLimitPhase,
    output: Option<&CommandOutput>,
) -> Result<ToolResult, ToolError> {
    let mut display = if phase.side_effects_applied() {
        format!(
            "Shell command completed, but change evidence could not be completed within the bounded snapshot. Changes may have occurred, and edit baselines under the snapshot scopes were invalidated.\n\n{diagnostic}"
        )
    } else {
        format!("Shell command was not executed.\n\n{diagnostic}")
    };
    if let Some(output) = output {
        display.push_str("\n\n");
        display.push_str(&format_shell_output_for_display(
            command,
            &output.stdout,
            &output.stderr,
            output.exit_code,
            output.timed_out,
            output.cancelled,
        ));
    }
    let preview = ctx.services.truncator.preview(
        display,
        &ctx.config.tool_output,
        &ctx.services.storage_paths,
    )?;
    let metadata = shell_snapshot_limit_metadata(phase, plan, limits, diagnostic, output);

    Ok(ToolResult {
        title: format!("Blocked: {description}"),
        output_text: preview.preview_text,
        metadata: merge_json_objects(
            metadata,
            json!({
                "truncated": preview.truncated,
            }),
        ),
        truncated_output_path: preview.truncated_output_path,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    })
}

fn shell_snapshot_limit_metadata(
    phase: SnapshotLimitPhase,
    plan: &ShellSnapshotPlan,
    limits: SnapshotLimits,
    diagnostic: &str,
    output: Option<&CommandOutput>,
) -> serde_json::Value {
    let exit_code = output.and_then(|value| value.exit_code);
    let timed_out = output.is_some_and(|value| value.timed_out);
    let cancelled = output.is_some_and(|value| value.cancelled);
    let stdout_present = output.is_some_and(|value| !value.stdout.trim().is_empty());
    let stderr_present = output.is_some_and(|value| !value.stderr.trim().is_empty());
    json!({
        "exit_code": exit_code,
        "timeout": timed_out,
        "cancelled": cancelled,
        "changed_files": [],
        "success": false,
        "snapshot_blocked": true,
        "snapshot_phase": phase.as_str(),
        "snapshot_owner": plan.owner_root,
        "snapshot_scopes": plan.scopes,
        "snapshot_limits": limits.metadata(),
        "blocked_reason": diagnostic,
        "side_effects_applied": phase.side_effects_applied(),
        "stdout_present": stdout_present,
        "stderr_present": stderr_present
    })
}

fn merge_json_objects(mut base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
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

// Local language-command token classification for UTF-8 encoding corrections on
// Windows / closed-network hosts. Inlined from the removed agent::language_evidence
// module; shell encoding review is the only remaining consumer.
const LANGUAGE_TEXT_IO_COMMAND_TOKENS: &[&str] = &[
    "bun", "cargo", "deno", "dotnet", "go", "gradle", "java", "javac", "mvn", "node", "npm",
    "perl", "php", "pnpm", "py", "pytest", "python", "python3", "ruby", "rustc", "unittest",
    "yarn",
];

const LANGUAGE_RUNTIME_EXECUTION_TOKENS: &[&str] = &[
    "bun", "deno", "java", "node", "perl", "php", "py", "python", "python3", "ruby",
];

const LANGUAGE_TEST_OR_VERIFICATION_TEXT_IO_TOKENS: &[&str] = &[
    "bun", "cargo", "deno", "dotnet", "go", "gradle", "jest", "mvn", "npm", "pnpm", "pytest",
    "test", "tests", "unittest", "vitest", "yarn",
];

const PYTHON_UTF8_BOOTSTRAP_TOKENS: &[&str] = &["py", "pytest", "python", "python3", "unittest"];

const LANGUAGE_VERIFICATION_COMMAND_PREFIXES: &[&str] = &[
    "bun test",
    "cargo build",
    "cargo check",
    "cargo test",
    "deno test",
    "dotnet test",
    "go test",
    "gradle test",
    "mvn test",
    "node --test",
    "npx jest",
    "npx vitest",
    "npm run test",
    "npm test",
    "pnpm run test",
    "pnpm test",
    "pytest",
    "python -m pytest",
    "python -m unittest",
    "python3 -m pytest",
    "python3 -m unittest",
    "yarn test",
];

fn python_direct_test_script_evidence(lower: &str) -> bool {
    (lower.contains("python") || lower.starts_with("py "))
        && (lower.contains("test_")
            || lower.contains("_test.py")
            || lower.contains("/tests/")
            || lower.contains("\\tests\\"))
}

fn language_verification_command_evidence(lower: &str) -> bool {
    LANGUAGE_VERIFICATION_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lower.contains(prefix))
        || python_direct_test_script_evidence(lower)
}

fn language_command_text_io_surface_evidence(tokens: &[String], lower: &str) -> bool {
    language_verification_command_evidence(lower)
        || tokens
            .iter()
            .any(|token| LANGUAGE_TEXT_IO_COMMAND_TOKENS.contains(&token.as_str()))
}

fn language_command_test_or_verification_io_evidence(tokens: &[String], lower: &str) -> bool {
    language_verification_command_evidence(lower)
        || tokens
            .iter()
            .any(|token| LANGUAGE_TEST_OR_VERIFICATION_TEXT_IO_TOKENS.contains(&token.as_str()))
}

fn language_runtime_execution_io_evidence(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| LANGUAGE_RUNTIME_EXECUTION_TOKENS.contains(&token.as_str()))
}

fn language_command_inherits_utf8_bootstrap(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| PYTHON_UTF8_BOOTSTRAP_TOKENS.contains(&token.as_str()))
}

fn language_python_utf8_correction_applies(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| matches!(token.as_str(), "python" | "python3" | "py" | "pytest"))
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
    cancelled: bool,
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
    let stdout = captured_shell_text(stdout_capture.bytes.as_slice(), stdout_capture.truncated);
    let mut stderr = captured_shell_text(stderr_capture.bytes.as_slice(), stderr_capture.truncated);
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

fn apply_shell_environment(command: &mut Command, shell: &crate::config::ShellConfig) {
    let mut captured = HashMap::new();
    for key in &shell.env_allowlist {
        if let Some(value) = std::env::var_os(key) {
            captured.insert(key.clone(), value);
        }
    }
    let injected = platform_bootstrap_env(&captured);

    command.env_clear();
    for key in &shell.env_allowlist {
        if let Some(value) = captured.get(key) {
            command.env(key, value);
        }
    }
    for (key, value) in injected {
        command.env(key, value);
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
            {
                continue;
            }
            if shell_path_candidate_is_inside_uri(command, candidate.start(), candidate.end()) {
                continue;
            }
            let value = candidate.as_str();
            let normalized = value.replace('/', "\\");
            let path = Utf8PathBuf::from(normalized);
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
            {
                continue;
            }
            if shell_path_candidate_is_inside_uri(command, candidate.start(), candidate.end()) {
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
            {
                continue;
            }
            if shell_path_candidate_is_inside_uri(command, candidate.start(), candidate.end()) {
                continue;
            }
            let value = candidate.as_str();
            let path = Utf8PathBuf::from(value);
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
    let normalized = value.replace('/', "\\");
    let path = Utf8PathBuf::from(normalized);
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
        format!("Workdir: {}", workdir),
        "Shell runs with the current user account; this review is risk classification, not an OS filesystem sandbox."
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

#[derive(Debug, Clone)]
struct SnapshotEntry {
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ShellSnapshotPlan {
    owner_root: Utf8PathBuf,
    scopes: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct SnapshotLimits {
    max_walk_entries: usize,
    max_files: usize,
    max_total_bytes: u64,
    max_file_bytes: u64,
}

impl SnapshotLimits {
    fn metadata(self) -> serde_json::Value {
        json!({
            "max_walk_entries": self.max_walk_entries,
            "max_files": self.max_files,
            "max_total_bytes": self.max_total_bytes,
            "max_file_bytes": self.max_file_bytes,
        })
    }
}

#[derive(Debug, Default)]
struct SnapshotBudget {
    walk_entries: usize,
    files: usize,
    total_bytes: u64,
}

#[derive(Debug)]
enum ShellSnapshotError {
    Limit(String),
    Other(ToolError),
}

impl std::fmt::Display for ShellSnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Limit(message) => formatter.write_str(message),
            Self::Other(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl From<ToolError> for ShellSnapshotError {
    fn from(error: ToolError) -> Self {
        Self::Other(error)
    }
}

impl From<std::io::Error> for ShellSnapshotError {
    fn from(error: std::io::Error) -> Self {
        Self::Other(ToolError::from(error))
    }
}

impl From<crate::error::WorkspaceError> for ShellSnapshotError {
    fn from(error: crate::error::WorkspaceError) -> Self {
        Self::Other(ToolError::from(error))
    }
}

fn shell_snapshot_owner(
    workspace: &crate::workspace::Workspace,
    guarded_workdir: &crate::workspace::GuardedPath,
) -> Utf8PathBuf {
    if guarded_workdir.inside_workspace {
        return workspace.root.clone();
    }

    workspace
        .path_policy
        .additional_write_roots
        .iter()
        .filter(|root| guarded_workdir.absolute.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned()
        .unwrap_or_else(|| guarded_workdir.absolute.clone())
}

fn shell_snapshot_plan(
    workspace: &crate::workspace::Workspace,
    guarded_workdir: &crate::workspace::GuardedPath,
    command: &str,
) -> ShellSnapshotPlan {
    let owner_root = shell_snapshot_owner(workspace, guarded_workdir);
    let mut scopes = vec![guarded_workdir.absolute.clone()];
    scopes.extend(extract_absolute_paths(&guarded_workdir.absolute, command));
    if command.contains("..") {
        scopes.push(owner_root.clone());
    }
    ShellSnapshotPlan {
        owner_root,
        scopes: normalize_snapshot_scopes(scopes),
    }
}

fn normalize_snapshot_scopes(mut scopes: Vec<Utf8PathBuf>) -> Vec<Utf8PathBuf> {
    scopes.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    scopes.dedup();
    let mut normalized = Vec::<Utf8PathBuf>::new();
    for scope in scopes {
        if normalized.iter().any(|owner| scope.starts_with(owner)) {
            continue;
        }
        normalized.push(scope);
    }
    normalized
}

fn snapshot_workspace(
    workspace: &crate::workspace::Workspace,
    plan: &ShellSnapshotPlan,
    limits: SnapshotLimits,
) -> Result<HashMap<Utf8PathBuf, SnapshotEntry>, ShellSnapshotError> {
    let ignore = workspace.ignore.compile()?;
    let mut snapshot = HashMap::new();
    let mut budget = SnapshotBudget::default();

    for scope in &plan.scopes {
        if scope.is_file() {
            snapshot_file(
                workspace,
                &ignore,
                scope,
                scope.parent().unwrap_or(scope),
                plan,
                limits,
                &mut budget,
                &mut snapshot,
            )?;
            continue;
        }
        if !scope.is_dir() {
            continue;
        }
        let mut builder = WalkBuilder::new(scope);
        builder.hidden(false);
        builder.git_ignore(workspace.ignore.use_gitignore);
        for entry in builder.build() {
            budget.walk_entries = budget.walk_entries.saturating_add(1);
            if budget.walk_entries > limits.max_walk_entries {
                return Err(snapshot_limit_error(plan, limits, "walk entry limit"));
            }
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
            snapshot_file(
                workspace,
                &ignore,
                &path,
                scope,
                plan,
                limits,
                &mut budget,
                &mut snapshot,
            )?;
        }
    }

    Ok(snapshot)
}

#[allow(clippy::too_many_arguments)]
fn snapshot_file(
    workspace: &crate::workspace::Workspace,
    ignore: &globset::GlobSet,
    path: &Utf8Path,
    scope_root: &Utf8Path,
    plan: &ShellSnapshotPlan,
    limits: SnapshotLimits,
    budget: &mut SnapshotBudget,
    snapshot: &mut HashMap<Utf8PathBuf, SnapshotEntry>,
) -> Result<(), ShellSnapshotError> {
    if snapshot.contains_key(path)
        || workspace
            .protected_paths
            .iter()
            .any(|value| path.starts_with(value))
        || workspace.ignore.matches_compiled(ignore, scope_root, path)
    {
        return Ok(());
    }

    if budget.files >= limits.max_files {
        return Err(snapshot_limit_error(plan, limits, "file count limit"));
    }
    let metadata = fs::metadata(path)?;
    let size_bytes = metadata.len();
    if size_bytes > limits.max_file_bytes {
        return Err(snapshot_limit_error(plan, limits, "single file byte limit"));
    }
    if budget.total_bytes.saturating_add(size_bytes) > limits.max_total_bytes {
        return Err(snapshot_limit_error(plan, limits, "total byte limit"));
    }

    let remaining_total = limits.max_total_bytes.saturating_sub(budget.total_bytes);
    let read_limit = limits.max_file_bytes.min(remaining_total);
    let mut bytes = Vec::with_capacity((size_bytes.min(read_limit)) as usize);
    fs::File::open(path)?
        .take(read_limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > read_limit {
        return Err(snapshot_limit_error(
            plan,
            limits,
            "file grew beyond byte limit while snapshotting",
        ));
    }

    budget.files += 1;
    budget.total_bytes = budget.total_bytes.saturating_add(bytes.len() as u64);
    snapshot.insert(path.to_path_buf(), SnapshotEntry { bytes });
    Ok(())
}

fn snapshot_limit_error(
    plan: &ShellSnapshotPlan,
    limits: SnapshotLimits,
    reason: &str,
) -> ShellSnapshotError {
    ShellSnapshotError::Limit(format!(
        "shell change snapshot exceeded its bounded scope ({reason}) for owner `{}`. Use a narrower `workdir` or a command with explicit target paths. Limits: {} walk entries, {} files, {} total bytes, {} bytes per file.",
        plan.owner_root,
        limits.max_walk_entries,
        limits.max_files,
        limits.max_total_bytes,
        limits.max_file_bytes,
    ))
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
        let before_text = before_entry.map(snapshot_entry_text).unwrap_or_default();
        let after_text = after_entry.map(snapshot_entry_text).unwrap_or_default();
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

fn snapshot_entry_text(entry: &SnapshotEntry) -> String {
    String::from_utf8(entry.bytes.clone()).unwrap_or_else(|_| "<<binary>>".to_string())
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
    let output = format_shell_output_for_display(
        "Get-Process",
        "powershell  123",
        "",
        Some(0),
        false,
        false,
    );
    let failed =
        format_shell_output_for_display("uv add ということ", "", "error", Some(1), false, false);
    output.contains("Command: Get-Process")
        && output.contains("Stdout:\npowershell  123")
        && output.contains("Stderr:\n(empty)")
        && failed.contains("Exit code: 1")
        && failed.contains("Recovery:")
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use tokio_util::sync::CancellationToken;

    use crate::config::ResolvedConfig;
    use crate::workspace::{AccessKind, PathGuard, WorkspaceDiscovery};

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

    #[test]
    fn network_urls_are_not_classified_as_outside_workspace_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");

        for command in [
            "curl.exe http://127.0.0.1:18945/health",
            "Invoke-WebRequest https://example.com/C:/artifact.json",
            "Invoke-WebRequest 'https://example.com/download?file=C:/artifact.json'",
        ] {
            assert!(
                !super::references_outside_workspace(&workspace, command),
                "URL must not be projected as an outside-workspace file path: {command}"
            );
            let risks = super::shell_permission_risks(&workspace, command);
            assert!(risks.contains(&crate::tool::PermissionRisk::Network));
            assert!(risks.contains(&crate::tool::PermissionRisk::ExternalConnection));
        }
    }

    #[test]
    fn actual_absolute_path_outside_workspace_is_still_classified_as_outside() {
        let workspace_temp = tempfile::tempdir().expect("workspace tempdir");
        let outside_temp = tempfile::tempdir().expect("outside tempdir");
        let root = Utf8PathBuf::from_path_buf(workspace_temp.path().to_path_buf())
            .expect("utf8 workspace");
        let outside = Utf8PathBuf::from_path_buf(outside_temp.path().join("outside.txt"))
            .expect("utf8 outside");
        let outside_with_spaces =
            Utf8PathBuf::from_path_buf(outside_temp.path().join("folder with spaces/outside.txt"))
                .expect("utf8 outside with spaces");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let command = format!("Get-Content -LiteralPath '{}'", outside);

        assert!(super::references_outside_workspace(&workspace, &command));
        let quoted_with_spaces = format!("Get-Content -LiteralPath '{}'", outside_with_spaces);
        assert!(super::references_outside_workspace(
            &workspace,
            &quoted_with_spaces
        ));

        let guarded = PathGuard::require_path(&workspace, &root, AccessKind::Shell)
            .expect("admit workspace workdir");
        let redirected = format!("Write-Output fixture >{}", outside);
        assert!(super::references_outside_workspace(&workspace, &redirected));
        let plan = super::shell_snapshot_plan(&workspace, &guarded, &redirected);
        assert!(plan.scopes.contains(&outside));
        let spaced_plan = super::shell_snapshot_plan(&workspace, &guarded, &quoted_with_spaces);
        assert!(spaced_plan.scopes.contains(&outside_with_spaces));
    }

    #[test]
    fn configured_external_write_root_is_not_classified_as_outside() {
        let workspace_temp = tempfile::tempdir().expect("workspace tempdir");
        let external_temp = tempfile::tempdir().expect("external tempdir");
        let root = Utf8PathBuf::from_path_buf(workspace_temp.path().to_path_buf())
            .expect("utf8 workspace");
        let external_root = Utf8PathBuf::from_path_buf(external_temp.path().to_path_buf())
            .expect("utf8 external root");
        let target = external_root.join("folder with spaces/allowed.txt");
        let mut config = ResolvedConfig::default();
        config.permissions.additional_write_roots = vec![external_root];
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let command = format!("Set-Content -LiteralPath '{}' -Value allowed", target);

        assert!(!super::references_outside_workspace(&workspace, &command));
        let guarded = PathGuard::require_path(&workspace, &root, AccessKind::Shell)
            .expect("admit workspace workdir");
        let plan = super::shell_snapshot_plan(&workspace, &guarded, &command);
        assert!(plan.scopes.contains(&target));
    }

    #[cfg(windows)]
    #[test]
    fn unc_paths_are_classified_as_outside_workspace_and_external_connections() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        for command in [
            r"Get-Content -LiteralPath \\server\share\artifact.txt",
            r#"Get-Content -LiteralPath "\\server\share\artifact.txt""#,
            r"Get-Content -LiteralPath //server/share/artifact.txt",
        ] {
            assert!(super::references_outside_workspace(&workspace, command));
            assert!(
                super::shell_permission_risks(&workspace, command)
                    .contains(&crate::tool::PermissionRisk::ExternalConnection)
            );
            assert!(
                super::shell_permission_risks(&workspace, command)
                    .contains(&crate::tool::PermissionRisk::Network)
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn uri_and_windows_drive_ambiguity_cannot_hide_outside_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside_temp = tempfile::tempdir().expect("outside tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let outside = Utf8PathBuf::from_path_buf(outside_temp.path().join("outside.txt"))
            .expect("utf8 outside");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let guarded = PathGuard::require_path(&workspace, &root, AccessKind::Shell)
            .expect("admit workspace workdir");
        let drive_root = Utf8PathBuf::from("C:/outside.txt");
        let cases = [
            ("Remove-Item C://outside.txt".to_string(), drive_root),
            (format!("Remove-Item file://fixture,{}", outside), outside),
        ];

        for (command, expected_target) in cases {
            let outside_workspace = super::references_outside_workspace(&workspace, &command);
            let risks = super::shell_permission_risks(&workspace, &command);
            let request = crate::tool::PermissionRequest {
                access: AccessKind::Shell,
                summary: command.clone(),
                details: Vec::new(),
                targets: vec![root.clone()],
                outside_workspace,
                risks,
                agent_path: None,
                agent_task_name: None,
            };
            let plan = super::shell_snapshot_plan(&workspace, &guarded, &command);

            assert!(outside_workspace, "outside path was hidden: {command}");
            assert!(!crate::tool::context::access_mode_allows_permission(
                crate::config::AccessMode::FullAccess,
                &request
            ));
            assert!(
                plan.scopes.contains(&expected_target),
                "snapshot scope omitted {expected_target} for {command}: {:?}",
                plan.scopes
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_device_and_comma_paths_cannot_bypass_boundary_review() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside_temp = tempfile::tempdir().expect("outside tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let outside = Utf8PathBuf::from_path_buf(outside_temp.path().join("outside.txt"))
            .expect("utf8 outside");
        let inside = root.join("inside.txt");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");

        for command in [
            format!(r"Get-Item {},{}", inside, outside),
            r"Get-Content -LiteralPath \\?\C:\outside.txt".to_string(),
            r"Get-Content -LiteralPath \\.\C:\outside.txt".to_string(),
        ] {
            assert!(
                super::references_outside_workspace(&workspace, &command),
                "path form must remain outside the workspace boundary: {command}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_workdir_drive_owns_drive_relative_path_resolution() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let external_root = Utf8PathBuf::from("D:/allowed");
        let workdir = external_root.join("nested");
        let mut config = ResolvedConfig::default();
        config.permissions.additional_write_roots = vec![external_root];
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let guarded = PathGuard::require_path(&workspace, &workdir, AccessKind::Shell)
            .expect("admit configured external workdir");
        let cases = [(
            "Remove-Item E:outside.txt".to_string(),
            Utf8PathBuf::from("E:/outside.txt"),
        )];

        for (command, expected_target) in cases {
            let outside_workspace =
                super::references_outside_workspace_from(&workspace, &guarded.absolute, &command);
            let risks = super::shell_permission_risks_from(&workspace, &guarded.absolute, &command);
            let request = crate::tool::PermissionRequest {
                access: AccessKind::Shell,
                summary: command.clone(),
                details: Vec::new(),
                targets: vec![guarded.absolute.clone()],
                outside_workspace,
                risks,
                agent_path: None,
                agent_task_name: None,
            };
            let plan = super::shell_snapshot_plan(&workspace, &guarded, &command);

            assert!(outside_workspace, "outside path was hidden: {command}");
            assert!(!crate::tool::context::access_mode_allows_permission(
                crate::config::AccessMode::FullAccess,
                &request
            ));
            assert!(
                plan.scopes.contains(&expected_target),
                "snapshot scope omitted {expected_target} for {command}: {:?}",
                plan.scopes
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_native_slash_options_and_division_are_not_file_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");

        for command in [
            "cmd.exe /c dir",
            "cmd.exe \"/c\" dir",
            "robocopy source destination /e",
            "Write-Output (10 / 2)",
        ] {
            assert!(
                !super::references_outside_workspace(&workspace, command),
                "slash option/operator must not be projected as a path: {command}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn unc_workdir_requires_external_review() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let workdir = Utf8PathBuf::from(r"\\server\share\project");
        let relative_risks =
            super::shell_permission_risks_from(&workspace, &workdir, "Get-ChildItem .");
        assert!(relative_risks.contains(&crate::tool::PermissionRisk::Network));
        assert!(relative_risks.contains(&crate::tool::PermissionRisk::ExternalConnection));
    }

    #[test]
    fn shell_confirmation_discloses_current_user_execution_boundary() {
        let details =
            super::shell_permission_details("Get-Date", camino::Utf8Path::new("C:/workspace"));

        assert_eq!(details[0], "Command: Get-Date");
        assert_eq!(details[1], "Workdir: C:/workspace");
        assert!(details[2].contains("current user account"));
        assert!(details[2].contains("not an OS filesystem sandbox"));
    }

    #[test]
    fn shell_timeout_termination_starts_with_process_tree_kill() {
        let plan = super::shell_timeout_termination_plan();
        assert_eq!(
            plan,
            vec![
                super::ShellTerminationStep::ProcessTreeKill,
                super::ShellTerminationStep::ParentStartKill,
                super::ShellTerminationStep::WaitForParent,
            ]
        );
    }

    #[test]
    fn external_write_root_owns_shell_snapshot() {
        let workspace_temp = tempfile::tempdir().expect("workspace tempdir");
        let external_temp = tempfile::tempdir().expect("external tempdir");
        let workspace_root = Utf8PathBuf::from_path_buf(workspace_temp.path().to_path_buf())
            .expect("utf8 workspace");
        let external_root = Utf8PathBuf::from_path_buf(external_temp.path().to_path_buf())
            .expect("utf8 external root");
        let workdir = external_root.join("nested");
        std::fs::create_dir_all(&workdir).expect("create external workdir");
        let mut config = ResolvedConfig::default();
        config.permissions.additional_write_roots = vec![external_root.clone()];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("discover workspace");
        let guarded = PathGuard::require_path(&workspace, &workdir, AccessKind::Shell)
            .expect("admit external workdir");

        assert_eq!(
            super::shell_snapshot_owner(&workspace, &guarded),
            external_root
        );
        let plan = super::shell_snapshot_plan(&workspace, &guarded, "Get-Date");
        assert_eq!(plan.owner_root, external_root);
        assert_eq!(plan.scopes, vec![workdir]);
    }

    #[test]
    fn shell_snapshot_scans_workdir_without_walking_large_workspace_siblings() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let workdir = root.join("focused");
        let sibling = root.join("large-sibling");
        std::fs::create_dir_all(&workdir).expect("create focused workdir");
        std::fs::create_dir_all(&sibling).expect("create sibling");
        std::fs::write(workdir.join("tracked.txt"), "tracked").expect("write tracked file");
        for index in 0..20 {
            std::fs::write(sibling.join(format!("ignored-{index}.txt")), "ignored")
                .expect("write sibling fixture");
        }
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let guarded = PathGuard::require_path(&workspace, &workdir, AccessKind::Shell)
            .expect("admit workdir");
        let plan = super::shell_snapshot_plan(&workspace, &guarded, "Get-Date");
        let limits = super::SnapshotLimits {
            max_walk_entries: 3,
            max_files: 2,
            max_total_bytes: 1_024,
            max_file_bytes: 1_024,
        };

        let snapshot =
            super::snapshot_workspace(&workspace, &plan, limits).expect("bounded snapshot");

        assert_eq!(plan.owner_root, root);
        assert_eq!(plan.scopes, vec![workdir.clone()]);
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot.contains_key(&workdir.join("tracked.txt")));
    }

    #[test]
    fn shell_snapshot_rejects_oversized_scope_before_command_execution() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for index in 0..3 {
            std::fs::write(root.join(format!("file-{index}.txt")), "content")
                .expect("write fixture");
        }
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let guarded =
            PathGuard::require_path(&workspace, &root, AccessKind::Shell).expect("admit workdir");
        let plan = super::shell_snapshot_plan(&workspace, &guarded, "Get-Date");
        let limits = super::SnapshotLimits {
            max_walk_entries: 10,
            max_files: 2,
            max_total_bytes: 1_024,
            max_file_bytes: 1_024,
        };

        let error = super::snapshot_workspace(&workspace, &plan, limits)
            .expect_err("oversized scope must fail before shell execution");

        assert!(error.to_string().contains("bounded scope"));
        assert!(error.to_string().contains("narrower `workdir`"));
        let metadata = super::shell_snapshot_limit_metadata(
            super::SnapshotLimitPhase::BeforeExecution,
            &plan,
            limits,
            &error.to_string(),
            None,
        );
        assert_eq!(metadata["success"].as_bool(), Some(false));
        assert_eq!(
            metadata["snapshot_phase"].as_str(),
            Some("before_execution")
        );
        assert_eq!(metadata["side_effects_applied"].as_bool(), Some(false));
    }

    #[test]
    fn parent_relative_command_expands_snapshot_to_owner_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let workdir = root.join("nested");
        std::fs::create_dir_all(&workdir).expect("create workdir");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let guarded = PathGuard::require_path(&workspace, &workdir, AccessKind::Shell)
            .expect("admit workdir");

        let plan =
            super::shell_snapshot_plan(&workspace, &guarded, "Set-Content ../outside.txt value");

        assert_eq!(plan.scopes, vec![root]);
    }

    #[test]
    fn post_execution_snapshot_limit_reports_blocked_possible_side_effects() {
        let plan = super::ShellSnapshotPlan {
            owner_root: Utf8PathBuf::from("C:/workspace"),
            scopes: vec![Utf8PathBuf::from("C:/workspace/focused")],
        };
        let limits = super::SnapshotLimits {
            max_walk_entries: 10,
            max_files: 2,
            max_total_bytes: 1_024,
            max_file_bytes: 1_024,
        };
        let output = super::CommandOutput {
            stdout: "created files".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            cancelled: false,
            stdout_truncated: false,
            stderr_truncated: false,
        };

        let metadata = super::shell_snapshot_limit_metadata(
            super::SnapshotLimitPhase::AfterExecution,
            &plan,
            limits,
            "file count limit",
            Some(&output),
        );

        assert_eq!(metadata["success"].as_bool(), Some(false));
        assert_eq!(metadata["snapshot_phase"].as_str(), Some("after_execution"));
        assert_eq!(metadata["side_effects_applied"].as_bool(), Some(true));
        assert_eq!(metadata["stdout_present"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn cancelled_shell_returns_output_so_after_snapshot_can_run() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("discover workspace");
        let guarded =
            PathGuard::require_path(&workspace, &root, AccessKind::Shell).expect("admit workdir");
        let plan = super::shell_snapshot_plan(&workspace, &guarded, cancelled_write_command());
        let before =
            super::snapshot_workspace(&workspace, &plan, super::DEFAULT_SHELL_SNAPSHOT_LIMITS)
                .expect("before snapshot");
        let changed_path = root.join("cancelled.txt");
        let cancel = CancellationToken::new();
        let cancel_after_change = cancel.clone();
        let watched_path = changed_path.clone();
        let canceller = tokio::spawn(async move {
            for _ in 0..300 {
                if watched_path.exists() {
                    cancel_after_change.cancel();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            cancel_after_change.cancel();
        });

        let output = super::execute_shell_command(
            &config.shell,
            &root,
            cancelled_write_command(),
            5_000,
            1_024,
            cancel,
        )
        .await
        .expect("cancel is a reportable shell outcome");
        canceller.await.expect("join canceller");
        let after =
            super::snapshot_workspace(&workspace, &plan, super::DEFAULT_SHELL_SNAPSHOT_LIMITS)
                .expect("after snapshot");

        assert!(output.cancelled);
        assert!(!before.contains_key(&changed_path));
        assert!(after.contains_key(&changed_path));
    }

    #[tokio::test]
    async fn pre_cancelled_shell_returns_without_starting_an_effect() {
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
        .expect("pre-cancel is a reportable shell outcome");

        assert!(output.cancelled);
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
        .expect("run bounded shell capture");

        assert!(output.stdout_truncated);
        assert!(output.stdout.starts_with("xxxxxxxx"));
        assert!(output.stdout.ends_with("[shell stream capture truncated]"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_shell_environment_is_allowlist_only() {
        let config = ResolvedConfig::default();
        let system_root = std::env::var("SystemRoot").expect("SystemRoot");
        let executable =
            Utf8PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = tokio::process::Command::new(executable);
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$secret=[Environment]::GetEnvironmentVariable('MOYAI_FORBIDDEN_TEST'); $system=[Environment]::GetEnvironmentVariable('SystemRoot'); [Console]::Out.Write(\"$secret|$system\")",
        ]);
        command.env("MOYAI_FORBIDDEN_TEST", "secret");

        super::apply_shell_environment(&mut command, &config.shell);
        let output = command
            .output()
            .await
            .expect("run filtered environment child");
        let stdout = String::from_utf8(output.stdout).expect("utf8 output");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.starts_with('|'));
        assert!(stdout[1..].eq_ignore_ascii_case(std::env::var("SystemRoot").unwrap().as_str()));
    }

    #[cfg(windows)]
    fn cancelled_write_command() -> &'static str {
        "Set-Content -LiteralPath cancelled.txt -Value changed -Encoding UTF8; Start-Sleep -Seconds 5"
    }

    #[cfg(not(windows))]
    fn cancelled_write_command() -> &'static str {
        "printf changed > cancelled.txt; sleep 5"
    }

    #[cfg(windows)]
    fn pre_cancelled_write_command() -> &'static str {
        "Set-Content -LiteralPath pre-cancelled.txt -Value changed -Encoding UTF8"
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
