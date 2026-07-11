use std::fs;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::config::ToolOutputConfig;
use crate::error::ToolError;
use crate::storage::StoragePaths;
use crate::tool::TruncatedToolOutput;

#[derive(Debug, Clone, Default)]
pub struct ToolTruncator;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedPipeOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

pub(crate) async fn read_pipe_bounded<T>(
    mut pipe: T,
    max_bytes: usize,
) -> Result<BoundedPipeOutput, std::io::Error>
where
    T: AsyncRead + Unpin,
{
    let mut stored = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(stored.len());
        let retained = remaining.min(read);
        stored.extend_from_slice(&chunk[..retained]);
        truncated |= retained < read;
    }
    Ok(BoundedPipeOutput {
        bytes: stored,
        truncated,
    })
}

impl ToolTruncator {
    pub fn preview(
        &self,
        text: String,
        limits: &ToolOutputConfig,
        paths: &StoragePaths,
    ) -> Result<TruncatedToolOutput, ToolError> {
        let total_bytes = text.len();
        let lines = text.lines().collect::<Vec<_>>();
        let exceeds = total_bytes > limits.max_bytes || lines.len() > limits.max_lines;
        if !exceeds {
            return Ok(TruncatedToolOutput {
                preview_text: text,
                truncated_output_path: None,
                truncated: false,
            });
        }

        fs::create_dir_all(&paths.truncation_dir)?;
        let file_name = format!("{}.txt", ulid::Ulid::new());
        let output_path = paths.truncation_dir.join(file_name);
        fs::write(&output_path, &text)?;

        let mut preview_lines = lines
            .into_iter()
            .take(limits.max_lines)
            .collect::<Vec<_>>()
            .join("\n");
        if preview_lines.len() > limits.max_bytes {
            truncate_to_char_boundary(&mut preview_lines, limits.max_bytes);
        }
        preview_lines.push_str(&format!(
            "\n[output truncated]\nFull output saved to: {}\n{}",
            output_path,
            truncation_followup_guidance()
        ));

        Ok(TruncatedToolOutput {
            preview_text: preview_lines,
            truncated_output_path: Some(output_path),
            truncated: true,
        })
    }
}

fn truncation_followup_guidance() -> &'static str {
    "Use `read` with `offset`/`limit`, or use registered `grep` with `path` set to that saved file, instead of rereading the full output."
}

pub fn truncate_to_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
}

pub fn clip_text_to_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut clipped = text.to_string();
    truncate_to_char_boundary(&mut clipped, max_bytes);
    clipped
}

pub fn clip_text_with_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes <= 3 {
        return clip_text_to_char_boundary(text, max_bytes);
    }
    let mut clipped = clip_text_to_char_boundary(text, max_bytes - 3)
        .trim_end()
        .to_string();
    clipped.push_str("...");
    clipped
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt as _;

    #[tokio::test]
    async fn bounded_pipe_reader_drains_without_retaining_excess_output() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let write = tokio::spawn(async move {
            writer
                .write_all(b"0123456789")
                .await
                .expect("write fixture");
        });

        let output = super::read_pipe_bounded(reader, 4)
            .await
            .expect("read bounded output");
        write.await.expect("join fixture writer");

        assert_eq!(output.bytes, b"0123");
        assert!(output.truncated);
    }
}
