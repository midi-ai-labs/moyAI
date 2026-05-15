use std::collections::HashMap;
use std::fs;
use std::process::Stdio;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

use crate::config::ShellFamily;
use crate::edit::path_for_change_storage;
use crate::error::ToolError;
use crate::session::ChangeRepository;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard, is_protected_instruction_or_config_path};

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
            PathGuard::require_path(ctx.workspace, &requested_workdir, AccessKind::Shell, false)?;
        if let Some(summary) = shell_contract_violation(&input.command, family) {
            return Ok(ToolResult {
                title: "Correct shell invocation".to_string(),
                output_text: summary,
                metadata: json!({
                    "exit_code": null,
                    "timeout": false,
                    "truncated": false,
                    "changed_files": [],
                    "corrective_result": true,
                }),
                truncated_output_path: None,
                recorded_changes: Vec::new(),
                change_summaries: Vec::new(),
            });
        }
        let outside_workspace = (!guarded.inside_workspace && !guarded.trusted_external)
            || references_outside_workspace(ctx.workspace, &input.command);
        let description = if input.description.trim().is_empty() {
            default_description(&input.command)
        } else {
            input.description.clone()
        };
        let risks = shell_permission_risks(ctx.workspace, &input.command);
        ctx.confirm_if_needed(
            AccessKind::Shell,
            description.clone(),
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
        let changes = build_shell_changes(&ctx, before, after)?;
        let change_ids = ctx
            .services
            .store
            .change_repo()
            .insert_changes(ctx.session.session.id, &changes)
            .await?;
        let change_summaries = changes
            .iter()
            .map(|change| crate::edit::ChangeSummary {
                change_id: change.id,
                kind: change.kind,
                path_before: change.path_before.clone(),
                path_after: change.path_after.clone(),
            })
            .collect::<Vec<_>>();
        let changed_paths = changes
            .iter()
            .flat_map(|change| {
                [change.path_before.clone(), change.path_after.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        ctx.services
            .edit_safety
            .invalidate_paths(ctx.session.session.id, &changed_paths)?;

        let merged_output = if output.stderr.is_empty() {
            output.stdout
        } else if output.stdout.is_empty() {
            output.stderr
        } else {
            format!("{}\n{}", output.stdout, output.stderr)
        };
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
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: change_ids,
            change_summaries,
        })
    }
}

fn shell_contract_violation(command: &str, family: ShellFamily) -> Option<String> {
    let trimmed = command.trim();
    if matches!(family, ShellFamily::PowerShell) {
        if trimmed.contains("&&") {
            return Some(
                "This `shell` tool is running Windows PowerShell, and PowerShell 5.1 does not support `&&`. Rewrite the call using raw PowerShell syntax only. Prefer the `workdir` field instead of `cd ... && ...`, and if command chaining must depend on prior success, use `cmd1; if ($?) { cmd2 }`."
                    .to_string(),
            );
        }
        if trimmed.contains("2>&1") {
            return Some(
                "Do not append `2>&1` when using this `shell` tool on Windows PowerShell. moyai already captures both stdout and stderr for you, and PowerShell 5.1 can turn native stderr redirection into `NativeCommandError` noise. Send the raw command directly, for example `python -m unittest`."
                    .to_string(),
            );
        }
        if trimmed
            .to_ascii_lowercase()
            .starts_with("powershell -command")
        {
            return Some(
                "Do not wrap the command in `powershell -Command` when using this tool. Send the raw PowerShell command text directly, and use the `workdir` field to choose the directory."
                    .to_string(),
            );
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("dir /") || lower.starts_with("dir\t/") {
            return Some(
                "This `shell` tool is running Windows PowerShell, and `dir /s /b` is CMD-style syntax. Do not use CMD switches here. Use targeted PowerShell syntax for a specific directory, or prefer `list`, `glob`, `grep`, and `read` for repository inspection instead of a broad shell relist."
                    .to_string(),
            );
        }
        if starts_with_linux_diagnostic(trimmed)
            || lower.contains("| head")
            || lower.contains("| tail")
        {
            return Some(
                "This `shell` tool is running Windows PowerShell. Do not use Linux diagnostics such as `top`, `htop`, `free`, `uptime`, or pipes to `head` / `tail` here. Rewrite the command in native PowerShell. For read-only Windows system diagnostics requested by the user, prefer commands such as `Get-CimInstance Win32_Processor | Select-Object Name, LoadPercentage`, `Get-CimInstance Win32_OperatingSystem | Select-Object TotalVisibleMemorySize, FreePhysicalMemory`, `Get-Process`, or a short `Get-Process` CPU delta sample."
                    .to_string(),
            );
        }
    }
    None
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
            let _ = child.start_kill();
            kill_process_tree(pid).await?;
            let _ = timeout(Duration::from_secs(5), child.wait()).await;
            return Err(ToolError::Message("shell command cancelled by user".to_string()));
        }
        result = timeout(Duration::from_millis(timeout_ms), child.wait()) => match result {
            Ok(result) => (result?, false),
            Err(_) => {
                let _ = child.start_kill();
                kill_process_tree(pid).await?;
                let status = timeout(Duration::from_secs(5), child.wait())
                    .await
                    .map_err(|_| {
                        ToolError::Message(
                            "shell command timed out and could not be terminated cleanly".to_string(),
                        )
                    })??;
                (status, true)
            }
        }
    };

    let stdout = String::from_utf8_lossy(&join_pipe(stdout_task, "stdout").await?).into_owned();
    let stderr_bytes = join_pipe(stderr_task, "stderr").await?;
    let stderr = if timed_out {
        if stderr_bytes.is_empty() {
            "command timed out".to_string()
        } else {
            format!(
                "{}\ncommand timed out",
                String::from_utf8_lossy(&stderr_bytes)
            )
        }
    } else {
        String::from_utf8_lossy(&stderr_bytes).into_owned()
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
        Vec::new()
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
    Ok(())
}

#[cfg(not(any(unix, windows)))]
async fn kill_process_tree(_pid: u32) -> Result<(), ToolError> {
    Ok(())
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
    if command_mentions_protected_target(workspace, command) {
        risks.push(PermissionRisk::ProtectedInstructionOrConfig);
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
        "moyai.toml",
        ".moyai/config.toml",
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
        .any(|path| is_protected_instruction_or_config_path(&workspace.root, &path))
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

fn build_shell_changes(
    ctx: &ToolContext<'_>,
    before: HashMap<Utf8PathBuf, SnapshotEntry>,
    after: HashMap<Utf8PathBuf, SnapshotEntry>,
) -> Result<Vec<crate::edit::FileChange>, ToolError> {
    let mut all_paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    all_paths.sort();
    all_paths.dedup();

    let mut changes = Vec::new();
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
    Ok(changes)
}
