use std::process::Stdio;

use camino::Utf8Path;
use globset::{Glob, GlobSetBuilder};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::{FormatConfig, FormatterRule, NewlineStyle};
use crate::error::EditError;

#[derive(Debug, Clone)]
pub struct Formatter {
    config: FormatConfig,
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

        let mut child = command.spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(text.as_bytes()).await?;
        }
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(EditError::Message(format!(
                "formatter `{}` failed: {}",
                rule.command.join(" "),
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        String::from_utf8(output.stdout)
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
