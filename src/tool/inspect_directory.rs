use std::collections::BTreeMap;
use std::fs;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::tool::{structured_document_guidance, structured_document_suggested_tools};
use crate::workspace::{AccessKind, PathGuard};

#[derive(Debug, Deserialize)]
pub struct InspectDirectoryInput {
    pub path: Option<Utf8PathBuf>,
    pub max_depth: Option<usize>,
    pub max_entries_per_dir: Option<usize>,
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Default)]
pub struct InspectDirectoryTool;

#[derive(Debug, Default)]
struct InspectionReport {
    tree_lines: Vec<String>,
    extension_counts: BTreeMap<String, usize>,
    large_file_candidates: Vec<LargeFileCandidate>,
    directories: usize,
    files: usize,
    omitted_entries: usize,
    depth_limited_directories: usize,
    max_depth_seen: usize,
}

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
            description: "Inspect a directory tree without reading file contents. Returns a tree preview, extension distribution, and large-file candidates.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "max_depth": { "type": "integer" },
                    "max_entries_per_dir": { "type": "integer" },
                    "include_hidden": { "type": "boolean" }
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

        if !guarded.absolute.exists() {
            return Ok(corrective_result(
                "Directory inspection target does not exist",
                &format!(
                    "`{}` does not exist yet. Inspect the nearest existing parent directory, or create the intended file directly with `write` if the user already specified the target path.",
                    guarded.absolute
                ),
                json!({
                    "corrective_result": true,
                    "missing_directory": true,
                    "path": guarded.absolute,
                }),
            ));
        }
        if guarded.absolute.is_file() {
            let suggested_tools = structured_document_suggested_tools(ctx.config);
            return Ok(corrective_result(
                "Directory inspection target is a file",
                &format!(
                    "`{}` is a file, not a directory. Use `read` for text files. For structured documents such as PDF or DOCX, {}",
                    guarded.absolute,
                    structured_document_guidance(ctx.config)
                ),
                json!({
                    "corrective_result": true,
                    "path_is_file": true,
                    "path": guarded.absolute,
                    "suggested_tools": suggested_tools,
                }),
            ));
        }

        let max_depth = input
            .max_depth
            .unwrap_or(ctx.config.inspection.default_max_depth);
        let max_entries_per_dir = input
            .max_entries_per_dir
            .unwrap_or(ctx.config.inspection.default_max_entries_per_dir)
            .max(1);
        let include_hidden = input
            .include_hidden
            .unwrap_or(ctx.config.inspection.include_hidden_by_default);

        let ignore = ctx.workspace.ignore.compile()?;
        let mut report = InspectionReport::default();
        report.tree_lines.push(format!("{}/", guarded.absolute));
        inspect_directory_recursive(
            &guarded.absolute,
            &guarded.absolute,
            0,
            max_depth,
            max_entries_per_dir,
            include_hidden,
            ctx.workspace,
            &ignore,
            ctx.config.file_guard.large_file_warning_bytes,
            &mut report,
        )?;

        let output_text = render_report(
            &guarded.absolute,
            &report,
            max_entries_per_dir,
            max_depth,
            ctx.config.inspection.max_extensions_reported,
            ctx.config.file_guard.large_file_warning_bytes,
            include_hidden,
        );
        let preview = ctx.services.truncator.preview(
            output_text,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Inspected {}", guarded.absolute),
            output_text: preview.preview_text,
            metadata: json!({
                "path": guarded.absolute,
                "directory_count": report.directories,
                "file_count": report.files,
                "omitted_entries": report.omitted_entries,
                "depth_limited_directories": report.depth_limited_directories,
                "max_depth_seen": report.max_depth_seen,
                "truncated": preview.truncated,
                "large_file_candidates": report.large_file_candidates.iter().map(|candidate| {
                    json!({
                        "path": candidate.path,
                        "size_bytes": candidate.size_bytes,
                    })
                }).collect::<Vec<_>>(),
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn inspect_directory_recursive(
    root: &Utf8Path,
    current: &Utf8Path,
    depth: usize,
    max_depth: usize,
    max_entries_per_dir: usize,
    include_hidden: bool,
    workspace: &crate::workspace::Workspace,
    ignore: &globset::GlobSet,
    large_file_warning_bytes: u64,
    report: &mut InspectionReport,
) -> Result<(), ToolError> {
    report.max_depth_seen = report.max_depth_seen.max(depth);
    let mut entries = fs::read_dir(current)?
        .map(|entry| {
            let entry = entry?;
            let path = Utf8PathBuf::from_path_buf(entry.path())
                .map_err(|_| ToolError::Message("path is not valid UTF-8".to_string()))?;
            Ok((path, entry.file_type()?))
        })
        .collect::<Result<Vec<_>, ToolError>>()?;

    entries.retain(|(path, _)| {
        if !include_hidden && path.file_name().is_some_and(|name| name.starts_with('.')) {
            return false;
        }
        if workspace
            .protected_paths
            .iter()
            .any(|protected| path.starts_with(protected))
        {
            return false;
        }
        !workspace
            .ignore
            .matches_compiled(ignore, &workspace.root, path.as_path())
    });
    entries.sort_by(|left, right| {
        let left_dir = left.1.is_dir();
        let right_dir = right.1.is_dir();
        right_dir
            .cmp(&left_dir)
            .then_with(|| left.0.as_str().cmp(right.0.as_str()))
    });

    let omitted = entries.len().saturating_sub(max_entries_per_dir);
    report.omitted_entries += omitted;
    for (path, file_type) in entries.into_iter().take(max_entries_per_dir) {
        let name = path
            .file_name()
            .ok_or_else(|| ToolError::Message("failed to derive file name".to_string()))?;
        let indent = "  ".repeat(depth + 1);
        if file_type.is_dir() {
            report.directories += 1;
            report.tree_lines.push(format!("{indent}{name}/"));
            if depth < max_depth {
                inspect_directory_recursive(
                    root,
                    &path,
                    depth + 1,
                    max_depth,
                    max_entries_per_dir,
                    include_hidden,
                    workspace,
                    ignore,
                    large_file_warning_bytes,
                    report,
                )?;
            } else {
                report.depth_limited_directories += 1;
                report
                    .tree_lines
                    .push(format!("{indent}  ... depth limit reached"));
            }
            continue;
        }

        report.files += 1;
        let metadata = fs::metadata(path.as_std_path())?;
        let size_bytes = metadata.len();
        report
            .tree_lines
            .push(format!("{indent}{name} ({})", human_size(size_bytes)));
        let extension = path
            .extension()
            .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
            .unwrap_or_else(|| "(no extension)".to_string());
        *report.extension_counts.entry(extension).or_insert(0) += 1;
        if size_bytes >= large_file_warning_bytes {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path.as_path())
                .as_str()
                .replace('\\', "/");
            report.large_file_candidates.push(LargeFileCandidate {
                path: relative,
                size_bytes,
            });
        }
    }

    if omitted > 0 {
        let indent = "  ".repeat(depth + 1);
        report
            .tree_lines
            .push(format!("{indent}... {omitted} more entries omitted"));
    }
    Ok(())
}

fn render_report(
    root: &Utf8Path,
    report: &InspectionReport,
    max_entries_per_dir: usize,
    max_depth: usize,
    max_extensions_reported: usize,
    large_file_warning_bytes: u64,
    include_hidden: bool,
) -> String {
    let mut lines = vec![
        format!("Directory inspection for `{root}`"),
        format!(
            "Summary: {} directories, {} files, max depth seen {}, hidden files {}",
            report.directories,
            report.files,
            report.max_depth_seen,
            if include_hidden {
                "included"
            } else {
                "excluded"
            }
        ),
        format!(
            "Limits: max_depth={max_depth}, max_entries_per_dir={max_entries_per_dir}, omitted_entries={}",
            report.omitted_entries
        ),
        String::new(),
        "Tree preview:".to_string(),
    ];
    lines.extend(report.tree_lines.iter().cloned());

    lines.push(String::new());
    lines.push("Extension distribution:".to_string());
    let mut extensions = report
        .extension_counts
        .iter()
        .map(|(extension, count)| (extension.as_str(), *count))
        .collect::<Vec<_>>();
    extensions.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    if extensions.is_empty() {
        lines.push("- no files found".to_string());
    } else {
        for (extension, count) in extensions.into_iter().take(max_extensions_reported) {
            lines.push(format!("- {extension}: {count}"));
        }
    }

    lines.push(String::new());
    lines.push(format!(
        "Large-file candidates (>= {}):",
        human_size(large_file_warning_bytes)
    ));
    if report.large_file_candidates.is_empty() {
        lines.push("- none".to_string());
    } else {
        let mut candidates = report.large_file_candidates.clone();
        candidates.sort_by(|left, right| {
            right
                .size_bytes
                .cmp(&left.size_bytes)
                .then_with(|| left.path.cmp(&right.path))
        });
        for candidate in candidates.into_iter().take(12) {
            lines.push(format!(
                "- {} ({})",
                candidate.path,
                human_size(candidate.size_bytes)
            ));
        }
    }
    lines.join("\n")
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
