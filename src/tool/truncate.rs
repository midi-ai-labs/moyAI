use std::fs;

use crate::config::ToolOutputConfig;
use crate::error::ToolError;
use crate::storage::StoragePaths;
use crate::tool::TruncatedToolOutput;

#[derive(Debug, Clone, Default)]
pub struct ToolTruncator;

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
            "\n[output truncated]\nFull output saved to: {}\nUse `read` with `offset`/`limit` or `grep` on that saved file instead of rereading the full output.",
            output_path
        ));

        Ok(TruncatedToolOutput {
            preview_text: preview_lines,
            truncated_output_path: Some(output_path),
            truncated: true,
        })
    }
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
