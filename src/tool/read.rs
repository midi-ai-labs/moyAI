use std::fs;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::runtime::SystemClock;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::tool::{structured_document_guidance, structured_document_suggested_tools};
use crate::workspace::{AccessKind, PathGuard, instruction_file_names};

#[derive(Debug, Deserialize)]
pub struct ReadInput {
    pub path: Utf8PathBuf,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Default)]
pub struct ReadTool;

#[async_trait(?Send)]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Read,
            description: "Read a UTF-8 text file with line numbers. Directories, binary files, large files, checkpoints, and structured documents are redirected to safer workflows.",
            input_schema: json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<ReadInput>(raw_arguments)?;
        let guarded = PathGuard::require_path(ctx.workspace, &input.path, AccessKind::Read)?;
        ctx.confirm_if_needed(
            AccessKind::Read,
            format!("Read {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )?;

        if guarded.absolute.is_dir() {
            return Ok(corrective_result(
                "Read redirected to directory inspection",
                &format!(
                    "`{}` is a directory. Use `inspect_directory` for a metadata-first tree and extension summary, or `list` when you only need a flat entry list.",
                    guarded.absolute
                ),
                json!({
                    "corrective_result": true,
                    "path": guarded.absolute,
                    "blocked_reason": "directory",
                    "suggested_tools": ["inspect_directory", "list"],
                }),
            ));
        }

        let metadata = fs::metadata(&guarded.absolute)?;
        let size_bytes = metadata.len();
        let extension = normalized_extension(&guarded.absolute);
        let blocked_extensions =
            normalized_extension_list(&ctx.config.file_guard.blocked_read_extensions);
        let structured_extensions =
            normalized_extension_list(&ctx.config.file_guard.structured_document_extensions);

        if blocked_extensions.iter().any(|value| value == &extension) {
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "checkpoint_or_binary_artifact",
                "This file matches the configured blocked extension list for large artifacts such as model checkpoints. Do not read it inline. Use `inspect_directory` to keep working from metadata only.",
                json!({
                    "extension": extension,
                    "suggested_tools": ["inspect_directory"],
                }),
            ));
        }

        if structured_extensions
            .iter()
            .any(|value| value == &extension)
        {
            let suggested_tools = structured_document_suggested_tools(ctx.config);
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "structured_document",
                &format!(
                    "This file is a structured document. Do not read it inline. {}",
                    structured_document_guidance(ctx.config)
                ),
                json!({
                    "extension": extension,
                    "suggested_tools": suggested_tools,
                }),
            ));
        }

        if size_bytes > ctx.config.file_guard.max_inline_read_bytes {
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "large_file",
                "This file exceeds the inline read limit. Do not keep retrying `read`. Use `inspect_directory` to stay metadata-first, or use a more specialized tool path if the user explicitly needs processing.",
                json!({
                    "max_inline_read_bytes": ctx.config.file_guard.max_inline_read_bytes,
                    "suggested_tools": ["inspect_directory"],
                }),
            ));
        }

        let bytes = fs::read(&guarded.absolute)?;
        if content_inspector::inspect(&bytes).is_binary() {
            let suggested_tools = structured_document_suggested_tools(ctx.config);
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "binary_content",
                &format!(
                    "This file is binary. Do not read it inline. {}",
                    structured_document_guidance(ctx.config)
                ),
                json!({
                    "extension": extension,
                    "suggested_tools": suggested_tools,
                }),
            ));
        }

        let text = String::from_utf8(bytes).map_err(|error| {
            ToolError::Message(format!(
                "file is not valid UTF-8 text after guard checks: {error}"
            ))
        })?;
        let lines = text.lines().collect::<Vec<_>>();
        let offset = input.offset.unwrap_or(1).max(1);
        let limit = input.limit.unwrap_or(2_000).max(1);
        let slice = lines
            .iter()
            .enumerate()
            .skip(offset - 1)
            .take(limit)
            .map(|(index, line)| format!("{}: {}", index + 1, line))
            .collect::<Vec<_>>();
        let output = slice.join("\n");
        let preview = ctx.services.truncator.preview(
            output,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as i64);
        ctx.services.edit_safety.record_read(
            ctx.session.session.id,
            crate::edit::FileReadStamp {
                path: guarded.absolute.clone(),
                read_at_ms: SystemClock::now_ms(),
                mtime_ms,
                size_bytes: Some(size_bytes),
            },
        )?;

        let instruction_sources = find_instruction_sources(&guarded.absolute, &ctx.workspace.root);
        Ok(ToolResult {
            title: format!("Read {}", guarded.absolute),
            output_text: preview.preview_text,
            metadata: json!({
                "path": guarded.absolute,
                "size_bytes": size_bytes,
                "start_line": offset,
                "end_line": (offset - 1) + slice.len(),
                "total_lines": lines.len(),
                "truncated": preview.truncated,
                "instruction_sources": instruction_sources,
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn read_blocked_result(
    path: &Utf8Path,
    size_bytes: u64,
    blocked_reason: &str,
    message: &str,
    extra_metadata: serde_json::Value,
) -> ToolResult {
    let mut metadata = json!({
        "corrective_result": true,
        "path": path,
        "size_bytes": size_bytes,
        "blocked_reason": blocked_reason,
    });
    if let (Some(target), Some(extra)) = (metadata.as_object_mut(), extra_metadata.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    corrective_result(
        &format!("Read blocked: {blocked_reason}"),
        &format!("`{path}` was not read inline.\n{message}"),
        metadata,
    )
}

fn corrective_result(title: &str, output_text: &str, metadata: serde_json::Value) -> ToolResult {
    ToolResult {
        title: title.to_string(),
        output_text: output_text.to_string(),
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

fn normalized_extension(path: &Utf8Path) -> String {
    path.extension()
        .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
        .unwrap_or_default()
}

fn normalized_extension_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn find_instruction_sources(path: &camino::Utf8Path, root: &camino::Utf8Path) -> Vec<String> {
    let mut sources = Vec::new();
    let mut current = path.parent();
    while let Some(dir) = current {
        for file_name in instruction_file_names() {
            let candidate = dir.join(file_name);
            if candidate.exists() {
                sources.push(candidate.as_str().replace('\\', "/"));
            }
        }
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    sources
}
