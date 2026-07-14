use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSetBuilder};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::config::{FormatConfig, FormatterRule, NewlineStyle};
use crate::error::EditError;
use crate::tool::process::{ManagedProcess, ManagedProcessOutput};

#[derive(Debug, Clone)]
pub struct Formatter {
    config: FormatConfig,
}

#[derive(Debug, Clone)]
pub struct FormatterExecutionOptions {
    pub workspace_root: Utf8PathBuf,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub cancel: CancellationToken,
}

impl Formatter {
    pub fn new(config: FormatConfig) -> Self {
        Self { config }
    }

    pub fn normalize_text(
        &self,
        _path: &Utf8Path,
        original: Option<&str>,
        edited: String,
    ) -> Result<String, EditError> {
        let newline = if let Some(value) = original {
            if value.contains("\r\n") { "\r\n" } else { "\n" }
        } else if matches!(self.config.default_newline, NewlineStyle::Crlf) {
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

        if self.config.ensure_trailing_newline
            && !normalized.is_empty()
            && !normalized.ends_with(newline)
        {
            normalized.push_str(newline);
        }

        Ok(normalized)
    }

    pub async fn format_if_configured(
        &self,
        path: &Utf8Path,
        text: String,
        options: FormatterExecutionOptions,
    ) -> Result<String, EditError> {
        let Some(rule) = self.matching_rule(path)? else {
            return Ok(text);
        };

        if rule.command.is_empty() {
            return Ok(text);
        }
        if options.cancel.is_cancelled() {
            return Err(EditError::Message(format!(
                "formatter `{}` cancelled by user",
                rule.command.join(" ")
            )));
        }

        let mut command = Command::new(&rule.command[0]);
        command.args(&rule.command[1..]);
        command.stdin(std::process::Stdio::piped());
        command.current_dir(formatter_working_directory(path, &options.workspace_root));
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
            return Err(stop.into_edit_error(&rule.command, options.timeout_ms, &cleanup));
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
                return Err(stop.into_edit_error(&rule.command, options.timeout_ms, &cleanup));
            }
        };
        if let Some(error) = completed.cleanup_error() {
            return Err(EditError::Message(format!(
                "formatter `{}` cleanup failed: {error}",
                rule.command.join(" ")
            )));
        }
        let status = completed.status.ok_or_else(|| {
            EditError::Message(format!(
                "formatter `{}` exited without a status",
                rule.command.join(" ")
            ))
        })?;
        let stdout = completed.stdout;
        let stderr = completed.stderr;
        if stdout.truncated || stderr.truncated {
            return Err(EditError::Message(format!(
                "formatter `{}` output exceeded the {} byte capture limit",
                rule.command.join(" "),
                output_limit
            )));
        }
        if !status.success() {
            return Err(EditError::Message(format!(
                "formatter `{}` failed: {}",
                rule.command.join(" "),
                String::from_utf8_lossy(&stderr.bytes)
            )));
        }

        String::from_utf8(stdout.bytes)
            .map_err(|error| EditError::Message(format!("formatter output is not UTF-8: {error}")))
    }

    fn matching_rule(&self, path: &Utf8Path) -> Result<Option<&FormatterRule>, EditError> {
        for rule in &self.config.commands {
            let mut builder = GlobSetBuilder::new();
            builder.add(
                Glob::new(&rule.glob).map_err(|error| {
                    EditError::Message(format!("invalid formatter glob: {error}"))
                })?,
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

    use super::{Formatter, FormatterExecutionOptions, formatter_working_directory};

    fn formatter(command: Vec<String>) -> Formatter {
        Formatter::new(FormatConfig {
            default_newline: NewlineStyle::Lf,
            ensure_trailing_newline: true,
            commands: vec![FormatterRule {
                glob: "**/*.txt".to_string(),
                command,
            }],
        })
    }

    fn options(workspace_root: Utf8PathBuf) -> FormatterExecutionOptions {
        FormatterExecutionOptions {
            workspace_root,
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

    #[tokio::test]
    async fn cancellation_is_propagated_to_formatter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut execution = options(root.clone());
        execution.cancel = cancel;

        let error = formatter(wait_command())
            .format_if_configured(&root.join("file.txt"), "input".to_string(), execution)
            .await
            .expect_err("cancelled formatter must fail");

        assert!(error.to_string().contains("cancelled by user"));
    }

    #[tokio::test]
    async fn formatter_timeout_terminates_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut execution = options(root.clone());
        execution.timeout_ms = 50;

        let error = formatter(wait_command())
            .format_if_configured(&root.join("file.txt"), "input".to_string(), execution)
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
        let mut execution = options(root.clone());
        execution.timeout_ms = 30_000;
        execution.cancel = cancel.clone();
        let formatter = formatter(delayed_grandchild_command(&marker, &ready));
        let marker_deadline = Instant::now() + Duration::from_millis(2_200);
        let target_for_task = target.clone();
        let task = tokio::spawn(async move {
            formatter
                .format_if_configured(&target_for_task, "input".to_string(), execution)
                .await
        });

        wait_for_file(&ready).await;
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
        let mut execution = options(root.clone());
        execution.timeout_ms = 750;
        let marker_deadline = Instant::now() + Duration::from_millis(2_200);

        let error = timeout(
            crate::tool::process::MANAGED_PROCESS_CLEANUP_GRACE + Duration::from_secs(2),
            formatter(delayed_grandchild_command(&marker, &ready)).format_if_configured(
                &target,
                "input".to_string(),
                execution,
            ),
        )
        .await
        .expect("formatter timeout cleanup must be bounded")
        .expect_err("timed out formatter must fail");
        sleep_until(marker_deadline).await;

        assert!(error.to_string().contains("timed out after 750 ms"));
        assert!(ready.exists(), "fixture never launched its grandchild");
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
        let mut execution = options(root.clone());
        execution.max_output_bytes = 32;

        let error = formatter(large_output_command())
            .format_if_configured(&root.join("file.txt"), "input".to_string(), execution)
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
    fn delayed_grandchild_command(marker: &Utf8PathBuf, ready: &Utf8PathBuf) -> Vec<String> {
        use base64::Engine as _;

        let marker = marker.as_str().replace('\'', "''");
        let ready = ready.as_str().replace('\'', "''");
        let child_script = format!(
            "Start-Sleep -Milliseconds 1500; [IO.File]::WriteAllText('{marker}', 'leaked')"
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
    fn delayed_grandchild_command(marker: &Utf8PathBuf, ready: &Utf8PathBuf) -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "(sleep 1.5; printf leaked > \"$1\") & printf ready > \"$2\"; cat >/dev/null; sleep 30"
                .to_string(),
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
