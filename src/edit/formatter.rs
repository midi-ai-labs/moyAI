use std::process::Stdio;

use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSetBuilder};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::config::{FormatConfig, FormatterRule, NewlineStyle};
use crate::error::EditError;
use crate::tool::truncate::{BoundedPipeOutput, read_pipe_bounded};

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

        let mut command = Command::new(&rule.command[0]);
        command.args(&rule.command[1..]);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.current_dir(formatter_working_directory(path, &options.workspace_root));
        command.kill_on_drop(true);

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EditError::Message("formatter stdout was not captured".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EditError::Message("formatter stderr was not captured".to_string()))?;
        let output_limit = options.max_output_bytes.max(1);
        let stdout_task = tokio::spawn(read_pipe_bounded(stdout, output_limit));
        let stderr_task = tokio::spawn(read_pipe_bounded(stderr, output_limit));
        let deadline = Instant::now() + Duration::from_millis(options.timeout_ms.max(1));

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| EditError::Message("formatter stdin was not captured".to_string()))?;
        let input_result = tokio::select! {
            _ = options.cancel.cancelled() => Err(FormatterStop::Cancelled),
            result = timeout_at(deadline, stdin.write_all(text.as_bytes())) => match result {
                Ok(result) => result.map_err(EditError::from).map_err(FormatterStop::Error),
                Err(_) => Err(FormatterStop::TimedOut),
            }
        };
        drop(stdin);
        if let Err(stop) = input_result {
            terminate_formatter(&mut child).await;
            let _ = join_formatter_pipe(stdout_task, "stdout").await;
            let _ = join_formatter_pipe(stderr_task, "stderr").await;
            return Err(stop.into_edit_error(&rule.command, options.timeout_ms));
        }

        let status = tokio::select! {
            _ = options.cancel.cancelled() => {
                terminate_formatter(&mut child).await;
                return Err(EditError::Message(format!(
                    "formatter `{}` cancelled by user",
                    rule.command.join(" ")
                )));
            }
            result = timeout_at(deadline, child.wait()) => match result {
                Ok(result) => result?,
                Err(_) => {
                    terminate_formatter(&mut child).await;
                    return Err(EditError::Message(format!(
                        "formatter `{}` timed out after {} ms",
                        rule.command.join(" "),
                        options.timeout_ms
                    )));
                }
            }
        };
        let stdout = join_formatter_pipe(stdout_task, "stdout").await?;
        let stderr = join_formatter_pipe(stderr_task, "stderr").await?;
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
    fn into_edit_error(self, command: &[String], timeout_ms: u64) -> EditError {
        match self {
            Self::Cancelled => EditError::Message(format!(
                "formatter `{}` cancelled by user",
                command.join(" ")
            )),
            Self::TimedOut => EditError::Message(format!(
                "formatter `{}` timed out after {timeout_ms} ms",
                command.join(" ")
            )),
            Self::Error(error) => error,
        }
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

async fn terminate_formatter(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

async fn join_formatter_pipe(
    task: JoinHandle<Result<BoundedPipeOutput, std::io::Error>>,
    label: &str,
) -> Result<BoundedPipeOutput, EditError> {
    task.await
        .map_err(|error| EditError::Message(format!("failed to join formatter {label}: {error}")))?
        .map_err(EditError::from)
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
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
