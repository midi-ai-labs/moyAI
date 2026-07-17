use std::collections::BTreeMap;
use std::fs;

use async_trait::async_trait;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::workspace::traversal::{TraversalOptions, walk_page};
use crate::workspace::{AccessKind, PathGuard};

#[derive(Debug, Deserialize)]
pub struct InspectDirectoryInput {
    pub path: Option<Utf8PathBuf>,
    pub max_depth: Option<usize>,
    pub limit: Option<usize>,
    pub include_hidden: Option<bool>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default)]
pub struct InspectDirectoryTool;

#[derive(Debug, Clone)]
struct LargeFileCandidate {
    path: String,
    size_bytes: u64,
}

#[async_trait(?Send)]
impl Tool for InspectDirectoryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::InspectDirectory,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Inspect one bounded metadata-only page of a directory tree.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "max_depth": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 },
                    "include_hidden": { "type": "boolean" },
                    "cursor": { "type": "string", "description": "Continuation returned by a previous inspect_directory call with the same path and options." }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<InspectDirectoryInput>(raw_arguments)?;
        let requested = input.path.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded = PathGuard::require_path(ctx.workspace, &requested, AccessKind::List)?;
        ctx.confirm_if_needed(
            AccessKind::List,
            format!("Inspect {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )
        .await?
        .admit()?;

        PathGuard::revalidate(&guarded)?;

        if !guarded.absolute.exists() {
            return Err(ToolError::Message(format!(
                "path `{}` does not exist",
                guarded.absolute
            )));
        }
        if !guarded.absolute.is_dir() {
            return Err(ToolError::Message(format!(
                "path `{}` is not a directory",
                guarded.absolute
            )));
        }

        let limit = input
            .limit
            .unwrap_or(ctx.config.tool_output.max_results.max(1))
            .max(1)
            .min(ctx.config.tool_output.max_results.max(1));
        let max_depth = input
            .max_depth
            .unwrap_or(ctx.config.inspection.default_max_depth)
            .max(1);
        let include_hidden = input
            .include_hidden
            .unwrap_or(ctx.config.inspection.include_hidden_by_default);
        let visit_limit = limit.saturating_mul(8).max(128).min(4_096);
        let page = walk_page(
            &guarded.absolute,
            ctx.workspace,
            input.cursor.as_deref(),
            TraversalOptions {
                include_hidden,
                max_depth: Some(max_depth),
                include_files: true,
                include_directories: true,
                result_limit: limit,
                visit_limit,
            },
        )?;

        let mut lines = Vec::new();
        let mut output_bytes = 0usize;
        let mut continuation = None;
        let mut directories = 0usize;
        let mut files = 0usize;
        let mut extension_counts = BTreeMap::<String, usize>::new();
        let mut large_file_candidates = Vec::new();
        let max_lines = ctx.config.tool_output.max_lines.max(1);
        let max_bytes = ctx.config.tool_output.max_bytes.max(1);

        for entry in &page.entries {
            let relative = entry.relative_path.as_str().replace('\\', "/");
            let (mut line, size_bytes) = if entry.is_directory {
                (format!("{relative}/"), None)
            } else {
                let size_bytes = fs::metadata(entry.path.as_std_path())?.len();
                (
                    format!("{relative} ({})", human_size(size_bytes)),
                    Some(size_bytes),
                )
            };
            if line.len() > max_bytes {
                line = clip_text_with_ellipsis(&line, max_bytes);
            }
            let separator = usize::from(!lines.is_empty());
            if lines.len() >= max_lines
                || output_bytes
                    .saturating_add(separator)
                    .saturating_add(line.len())
                    > max_bytes
            {
                continuation = Some(entry.cursor.clone());
                break;
            }
            output_bytes = output_bytes
                .saturating_add(separator)
                .saturating_add(line.len());
            lines.push(line);

            if entry.is_directory {
                directories = directories.saturating_add(1);
            } else {
                files = files.saturating_add(1);
                let extension = entry
                    .path
                    .extension()
                    .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
                    .unwrap_or_else(|| "(no extension)".to_string());
                *extension_counts.entry(extension).or_insert(0) += 1;
                if let Some(size_bytes) = size_bytes
                    .filter(|size| *size >= ctx.config.file_guard.large_file_warning_bytes)
                {
                    large_file_candidates.push(LargeFileCandidate {
                        path: relative,
                        size_bytes,
                    });
                }
            }
        }
        if continuation.is_none() {
            continuation = page.continuation;
        }
        large_file_candidates.sort_by(|left, right| {
            right
                .size_bytes
                .cmp(&left.size_bytes)
                .then_with(|| left.path.cmp(&right.path))
        });
        large_file_candidates.truncate(12);

        Ok(ToolResult {
            title: format!("Inspected {}", guarded.absolute),
            output_text: lines.join("\n"),
            metadata: json!({
                "path": guarded.absolute,
                "directory_count": directories,
                "file_count": files,
                "entry_count": lines.len(),
                "visited_entries": page.visited_entries,
                "max_depth": max_depth,
                "include_hidden": include_hidden,
                "extension_counts": extension_counts,
                "large_file_candidates": large_file_candidates.iter().map(|candidate| json!({
                    "path": candidate.path,
                    "size_bytes": candidate.size_bytes,
                })).collect::<Vec<_>>(),
                "continuation": continuation,
                "truncated": continuation.is_some(),
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

fn human_size(size_bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = size_bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", size_bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
