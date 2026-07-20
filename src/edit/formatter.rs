use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSetBuilder};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::config::{FormatConfig, FormatterRule, NewlineStyle};
use crate::error::EditError;
use crate::tool::os_sandbox::ProcessSandboxPlan;
use crate::tool::process::{ManagedProcess, ManagedProcessOutput};
use crate::tool::sandbox_process::{
    SandboxedProcessRequest, captured_process_environment, execute_workspace_write,
};

#[derive(Debug, Clone)]
pub struct Formatter;

#[derive(Debug, Clone)]
pub struct FormatterExecutionOptions {
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFormatterInvocation {
    target: Utf8PathBuf,
    working_directory: Utf8PathBuf,
    command: Vec<String>,
}

impl ResolvedFormatterInvocation {
    pub fn target(&self) -> &Utf8Path {
        &self.target
    }

    pub fn working_directory(&self) -> &Utf8Path {
        &self.working_directory
    }

    pub fn command(&self) -> &[String] {
        &self.command
    }

    pub fn permission_detail(&self) -> String {
        let argv = serde_json::to_string(&self.command)
            .expect("formatter command strings must serialize as JSON");
        format!(
            "configured formatter: target={} cwd={} argv={argv}",
            self.target, self.working_directory
        )
    }
}

impl Formatter {
    pub fn new(_config: FormatConfig) -> Self {
        Self
    }

    pub fn normalize_text(
        &self,
        config: &FormatConfig,
        _path: &Utf8Path,
        original: Option<&str>,
        edited: String,
    ) -> Result<String, EditError> {
        let newline = if let Some(value) = original {
            if value.contains("\r\n") { "\r\n" } else { "\n" }
        } else if matches!(config.default_newline, NewlineStyle::Crlf) {
            "\r\n"
        } else {
            "\n"
        };

        let mut normalized = edited
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .collect::<Vec<_>>()
            .join(newline);

        if config.ensure_trailing_newline
            && !normalized.is_empty()
            && !normalized.ends_with(newline)
        {
            normalized.push_str(newline);
        }

        Ok(normalized)
    }

    pub fn resolve_invocation(
        config: &FormatConfig,
        path: &Utf8Path,
        workspace_root: &Utf8Path,
    ) -> Result<Option<ResolvedFormatterInvocation>, EditError> {
        let Some(rule) = matching_rule(config, path)? else {
            return Ok(None);
        };
        if rule.command.is_empty() {
            return Ok(None);
        }
        Ok(Some(ResolvedFormatterInvocation {
            target: path.to_path_buf(),
            working_directory: formatter_working_directory(path, workspace_root).to_path_buf(),
            command: rule.command.clone(),
        }))
    }

    pub async fn format_resolved(
        &self,
        invocation: &ResolvedFormatterInvocation,
        text: String,
        options: FormatterExecutionOptions,
    ) -> Result<String, EditError> {
        self.format_resolved_with_sandbox(
            invocation,
            text,
            options,
            &crate::config::ResolvedConfig::default().shell,
            &ProcessSandboxPlan::Unrestricted,
        )
        .await
    }

    pub(crate) async fn format_resolved_with_sandbox(
        &self,
        invocation: &ResolvedFormatterInvocation,
        text: String,
        options: FormatterExecutionOptions,
        shell: &crate::config::ShellConfig,
        sandbox_plan: &ProcessSandboxPlan,
    ) -> Result<String, EditError> {
        if options.cancel.is_cancelled() {
            return Err(EditError::Message(format!(
                "formatter `{}` cancelled by user",
                invocation.command.join(" ")
            )));
        }

        let environment = captured_process_environment(shell);
        if let ProcessSandboxPlan::NoProcess = sandbox_plan {
            return Err(EditError::Sandbox(
                crate::tool::sandbox_process::SandboxExecutionError::InvalidProfile(
                    "formatter process was not authorized by this tool admission".to_string(),
                ),
            ));
        }
        if let ProcessSandboxPlan::WorkspaceWrite(profile) = sandbox_plan {
            let completed = execute_workspace_write(
                profile.clone(),
                SandboxedProcessRequest {
                    argv: invocation.command.clone(),
                    cwd: invocation.working_directory.clone(),
                    environment,
                    stdin: text.into_bytes(),
                    timeout_ms: options.timeout_ms.max(1),
                    max_output_bytes: options.max_output_bytes.max(1),
                    hide_window: shell.hide_windows,
                    cancel: options.cancel,
                },
            )
            .await?;
            let cleanup_error = completed.cleanup_error();
            let cleanup_suffix = cleanup_error
                .as_deref()
                .map(|error| format!("; cleanup failed: {error}"))
                .unwrap_or_default();
            if completed.cancelled {
                return Err(EditError::Message(format!(
                    "formatter `{}` cancelled by user{cleanup_suffix}",
                    invocation.command.join(" "),
                )));
            }
            if completed.timed_out {
                return Err(EditError::Message(format!(
                    "formatter `{}` timed out after {} ms{cleanup_suffix}",
                    invocation.command.join(" "),
                    options.timeout_ms
                )));
            }
            if let Some(error) = cleanup_error {
                return Err(EditError::Message(format!(
                    "formatter `{}` cleanup failed: {error}",
                    invocation.command.join(" ")
                )));
            }
            if completed.stdout.truncated || completed.stderr.truncated {
                return Err(EditError::Message(format!(
                    "formatter `{}` output exceeded the {} byte capture limit",
                    invocation.command.join(" "),
                    options.max_output_bytes.max(1)
                )));
            }
            if completed.exit_code != Some(0) {
                return Err(EditError::Message(format!(
                    "formatter `{}` failed with exit code {}: {}",
                    invocation.command.join(" "),
                    completed
                        .exit_code
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    String::from_utf8_lossy(&completed.stderr.bytes)
                )));
            }
            return String::from_utf8(completed.stdout.bytes).map_err(|error| {
                EditError::Message(format!("formatter output is not UTF-8: {error}"))
            });
        }

        let mut command = Command::new(&invocation.command[0]);
        command.args(&invocation.command[1..]);
        command.stdin(std::process::Stdio::piped());
        command.current_dir(&invocation.working_directory);
        command.env_clear();
        command.envs(environment);
        let output_limit = options.max_output_bytes.max(1);
        let mut process = ManagedProcess::spawn(command, false, output_limit).await?;
        let deadline = Instant::now() + Duration::from_millis(options.timeout_ms.max(1));

        let Some(mut stdin) = process.take_stdin() else {
            let cleanup = process.terminate().await;
            return Err(formatter_cleanup_error(
                "formatter stdin was not captured".to_string(),
                &cleanup,
            ));
        };
        let input_result = tokio::select! {
            _ = options.cancel.cancelled() => Err(FormatterStop::Cancelled),
            result = timeout_at(deadline, stdin.write_all(text.as_bytes())) => match result {
                Ok(result) => result.map_err(EditError::from).map_err(FormatterStop::Error),
                Err(_) => Err(FormatterStop::TimedOut),
            }
        };
        drop(stdin);
        if let Err(stop) = input_result {
            let cleanup = process.terminate().await;
            return Err(stop.into_edit_error(&invocation.command, options.timeout_ms, &cleanup));
        }

        let wait_result = tokio::select! {
            _ = options.cancel.cancelled() => Err(FormatterStop::Cancelled),
            result = timeout_at(deadline, process.wait()) => match result {
                Ok(result) => result.map_err(EditError::from).map_err(FormatterStop::Error),
                Err(_) => Err(FormatterStop::TimedOut),
            }
        };
        let completed = match wait_result {
            Ok(status) => process.finish_after_exit(status).await,
            Err(stop) => {
                let cleanup = process.terminate().await;
                return Err(stop.into_edit_error(
                    &invocation.command,
                    options.timeout_ms,
                    &cleanup,
                ));
            }
        };
        if let Some(error) = completed.cleanup_error() {
            return Err(EditError::Message(format!(
                "formatter `{}` cleanup failed: {error}",
                invocation.command.join(" ")
            )));
        }
        let status = completed.status.ok_or_else(|| {
            EditError::Message(format!(
                "formatter `{}` exited without a status",
                invocation.command.join(" ")
            ))
        })?;
        let stdout = completed.stdout;
        let stderr = completed.stderr;
        if stdout.truncated || stderr.truncated {
            return Err(EditError::Message(format!(
                "formatter `{}` output exceeded the {} byte capture limit",
                invocation.command.join(" "),
                output_limit
            )));
        }
        if !status.success() {
            return Err(EditError::Message(format!(
                "formatter `{}` failed: {}",
                invocation.command.join(" "),
                String::from_utf8_lossy(&stderr.bytes)
            )));
        }

        String::from_utf8(stdout.bytes)
            .map_err(|error| EditError::Message(format!("formatter output is not UTF-8: {error}")))
    }
}

fn matching_rule<'a>(
    config: &'a FormatConfig,
    path: &Utf8Path,
) -> Result<Option<&'a FormatterRule>, EditError> {
    for rule in &config.commands {
        let mut builder = GlobSetBuilder::new();
        builder.add(
            Glob::new(&rule.glob)
                .map_err(|error| EditError::Message(format!("invalid formatter glob: {error}")))?,
        );
        let glob = builder.build().map_err(|error| {
            EditError::Message(format!("failed to compile formatter glob: {error}"))
        })?;
        if glob.is_match(path.as_str()) {
            return Ok(Some(rule));
        }
    }
    Ok(None)
}

#[derive(Debug)]
enum FormatterStop {
    Cancelled,
    TimedOut,
    Error(EditError),
}

impl FormatterStop {
    fn into_edit_error(
        self,
        command: &[String],
        timeout_ms: u64,
        cleanup: &ManagedProcessOutput,
    ) -> EditError {
        let error = match self {
            Self::Cancelled => EditError::Message(format!(
                "formatter `{}` cancelled by user",
                command.join(" ")
            )),
            Self::TimedOut => EditError::Message(format!(
                "formatter `{}` timed out after {timeout_ms} ms",
                command.join(" ")
            )),
            Self::Error(error) => error,
        };
        match cleanup.cleanup_error() {
            Some(cleanup_error) => EditError::Message(format!(
                "{error}; subprocess cleanup failed: {cleanup_error}"
            )),
            None => error,
        }
    }
}

fn formatter_cleanup_error(message: String, cleanup: &ManagedProcessOutput) -> EditError {
    match cleanup.cleanup_error() {
        Some(cleanup_error) => EditError::Message(format!(
            "{message}; subprocess cleanup failed: {cleanup_error}"
        )),
        None => EditError::Message(message),
    }
}

fn formatter_working_directory<'a>(
    path: &'a Utf8Path,
    workspace_root: &'a Utf8Path,
) -> &'a Utf8Path {
    path.parent()
        .filter(|parent| parent.is_dir())
        .unwrap_or(workspace_root)
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use tokio::time::{Duration, Instant, sleep_until, timeout};
    use tokio_util::sync::CancellationToken;

    use crate::config::{FormatConfig, FormatterRule, NewlineStyle};

    use super::{
        Formatter, FormatterExecutionOptions, ResolvedFormatterInvocation,
        formatter_working_directory,
    };

    fn formatter_config(command: Vec<String>) -> FormatConfig {
        FormatConfig {
            default_newline: NewlineStyle::Lf,
            ensure_trailing_newline: true,
            commands: vec![FormatterRule {
                glob: "**/*.txt".to_string(),
                command,
            }],
        }
    }

    fn resolved_formatter(
        command: Vec<String>,
        target: &camino::Utf8Path,
        workspace_root: &camino::Utf8Path,
    ) -> (Formatter, ResolvedFormatterInvocation) {
        let config = formatter_config(command);
        let invocation = Formatter::resolve_invocation(&config, target, workspace_root)
            .expect("resolve formatter")
            .expect("matching formatter");
        (Formatter::new(config), invocation)
    }

    fn options() -> FormatterExecutionOptions {
        FormatterExecutionOptions {
            timeout_ms: 2_000,
            max_output_bytes: 1_024,
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn working_directory_prefers_existing_file_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let parent = root.join("nested");
        std::fs::create_dir_all(&parent).expect("create nested");

        assert_eq!(
            formatter_working_directory(&parent.join("file.txt"), &root),
            parent
        );
        assert_eq!(
            formatter_working_directory(&root.join("missing/file.txt"), &root),
            root
        );
    }

    #[test]
    fn resolved_invocation_owns_the_approved_command_and_working_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let target = root.join("missing/file.txt");
        let approved = vec!["approved-formatter".to_string(), "--fix".to_string()];
        let mut config = formatter_config(approved.clone());

        let invocation = Formatter::resolve_invocation(&config, &target, &root)
            .expect("resolve formatter")
            .expect("matching formatter");
        config.commands[0].command = vec!["replacement-formatter".to_string()];
        std::fs::create_dir_all(target.parent().expect("target parent"))
            .expect("create target parent after approval");

        assert_eq!(invocation.target(), target);
        assert_eq!(invocation.working_directory(), root);
        assert_eq!(invocation.command(), approved);
        assert!(
            invocation
                .permission_detail()
                .contains("approved-formatter")
        );
        assert!(
            !invocation
                .permission_detail()
                .contains("replacement-formatter")
        );
    }

    #[tokio::test]
    async fn cancellation_is_propagated_to_formatter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut execution = options();
        execution.cancel = cancel;
        let target = root.join("file.txt");
        let (formatter, invocation) = resolved_formatter(wait_command(), &target, &root);

        let error = formatter
            .format_resolved(&invocation, "input".to_string(), execution)
            .await
            .expect_err("cancelled formatter must fail");

        assert!(error.to_string().contains("cancelled by user"));
    }

    #[tokio::test]
    async fn formatter_timeout_terminates_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut execution = options();
        execution.timeout_ms = 50;
        let target = root.join("file.txt");
        let (formatter, invocation) = resolved_formatter(wait_command(), &target, &root);

        let error = formatter
            .format_resolved(&invocation, "input".to_string(), execution)
            .await
            .expect_err("timed out formatter must fail");

        assert!(error.to_string().contains("timed out after 50 ms"));
    }

    #[tokio::test]
    async fn formatter_cancellation_terminates_grandchild_before_delayed_effect() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let target = root.join("file.txt");
        let ready = root.join("grandchild-ready.txt");
        let marker = root.join("grandchild-effect.txt");
        std::fs::write(&target, "original").expect("write target fixture");
        let cancel = CancellationToken::new();
        let mut execution = options();
        execution.timeout_ms = 30_000;
        execution.cancel = cancel.clone();
        let (formatter, invocation) = resolved_formatter(
            delayed_grandchild_command(&marker, &ready, 1_500),
            &target,
            &root,
        );
        let task = tokio::spawn(async move {
            formatter
                .format_resolved(&invocation, "input".to_string(), execution)
                .await
        });

        wait_for_file(&ready).await;
        let marker_deadline = Instant::now() + Duration::from_millis(2_200);
        cancel.cancel();
        let error = timeout(
            crate::tool::process::MANAGED_PROCESS_CLEANUP_GRACE + Duration::from_secs(2),
            task,
        )
        .await
        .expect("formatter cancellation cleanup must be bounded")
        .expect("join formatter task")
        .expect_err("cancelled formatter must fail");
        sleep_until(marker_deadline).await;

        assert!(error.to_string().contains("cancelled by user"));
        assert!(ready.exists(), "fixture never launched its grandchild");
        assert!(
            !marker.exists(),
            "formatter grandchild survived cancellation and applied a delayed effect"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read unchanged target"),
            "original"
        );
    }

    #[tokio::test]
    async fn formatter_timeout_terminates_grandchild_before_delayed_effect() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let target = root.join("file.txt");
        let ready = root.join("grandchild-ready.txt");
        let marker = root.join("grandchild-effect.txt");
        std::fs::write(&target, "original").expect("write target fixture");
        let (timeout_ms, effect_delay_ms, observation_ms) = timeout_grandchild_timing();
        let mut execution = options();
        execution.timeout_ms = timeout_ms;
        let (formatter, invocation) = resolved_formatter(
            delayed_grandchild_command(&marker, &ready, effect_delay_ms),
            &target,
            &root,
        );

        let error = timeout(
            crate::tool::process::MANAGED_PROCESS_CLEANUP_GRACE + Duration::from_secs(2),
            formatter.format_resolved(&invocation, "input".to_string(), execution),
        )
        .await
        .expect("formatter timeout cleanup must be bounded")
        .expect_err("timed out formatter must fail");
        assert!(ready.exists(), "fixture never launched its grandchild");
        let marker_deadline = Instant::now() + Duration::from_millis(observation_ms);
        sleep_until(marker_deadline).await;

        assert!(
            error
                .to_string()
                .contains(&format!("timed out after {timeout_ms} ms"))
        );
        assert!(
            !marker.exists(),
            "formatter grandchild survived timeout and applied a delayed effect"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read unchanged target"),
            "original"
        );
    }

    #[tokio::test]
    async fn formatter_output_capture_is_bounded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut execution = options();
        execution.max_output_bytes = 32;
        let target = root.join("file.txt");
        let (formatter, invocation) = resolved_formatter(large_output_command(), &target, &root);

        let error = formatter
            .format_resolved(&invocation, "input".to_string(), execution)
            .await
            .expect_err("oversized formatter output must fail");

        assert!(error.to_string().contains("32 byte capture limit"));
    }

    async fn wait_for_file(path: &Utf8PathBuf) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for formatter fixture {path}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(windows)]
    fn timeout_grandchild_timing() -> (u64, u64, u64) {
        // The formatter deadline starts before the outer PowerShell process has
        // initialized. Under a parallel Windows test load, that startup plus
        // Start-Process can legitimately exceed a sub-second deadline before
        // the fixture creates its grandchild. Keep the delayed effect beyond
        // the formatter deadline, and observe for a full delay after cleanup.
        (3_000, 4_000, 4_700)
    }

    #[cfg(not(windows))]
    fn timeout_grandchild_timing() -> (u64, u64, u64) {
        (750, 1_500, 2_200)
    }

    #[cfg(windows)]
    fn delayed_grandchild_command(
        marker: &Utf8PathBuf,
        ready: &Utf8PathBuf,
        effect_delay_ms: u64,
    ) -> Vec<String> {
        use base64::Engine as _;

        let marker = marker.as_str().replace('\'', "''");
        let ready = ready.as_str().replace('\'', "''");
        let child_script = format!(
            "Start-Sleep -Milliseconds {effect_delay_ms}; [IO.File]::WriteAllText('{marker}', 'leaked')"
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            child_script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let parent_script = format!(
            "$child = Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile','-NonInteractive','-EncodedCommand','{encoded}') -WindowStyle Hidden -PassThru; [IO.File]::WriteAllText('{ready}', $child.Id.ToString()); Start-Sleep -Seconds 30"
        );
        vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            parent_script,
        ]
    }

    #[cfg(not(windows))]
    fn delayed_grandchild_command(
        marker: &Utf8PathBuf,
        ready: &Utf8PathBuf,
        effect_delay_ms: u64,
    ) -> Vec<String> {
        let effect_delay = format!("{}.{:03}", effect_delay_ms / 1_000, effect_delay_ms % 1_000);
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "(sleep {effect_delay}; printf leaked > \"$1\") & printf ready > \"$2\"; cat >/dev/null; sleep 30"
            ),
            "formatter-fixture".to_string(),
            marker.to_string(),
            ready.to_string(),
        ]
    }

    #[cfg(windows)]
    fn wait_command() -> Vec<String> {
        vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "Start-Sleep -Seconds 2".to_string(),
        ]
    }

    #[cfg(not(windows))]
    fn wait_command() -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), "sleep 2".to_string()]
    }

    #[cfg(windows)]
    fn large_output_command() -> Vec<String> {
        vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "[Console]::Out.Write(('x' * 2048))".to_string(),
        ]
    }

    #[cfg(not(windows))]
    fn large_output_command() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "head -c 2048 /dev/zero | tr '\\0' x".to_string(),
        ]
    }
}
