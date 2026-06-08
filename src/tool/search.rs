use std::cmp::Reverse;
use std::fs;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard};

#[derive(Debug, Deserialize)]
pub struct ListInput {
    pub path: Option<Utf8PathBuf>,
    pub limit: Option<usize>,
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct GlobInput {
    pub pattern: String,
    pub path: Option<Utf8PathBuf>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct GrepInput {
    pub pattern: String,
    pub path: Option<Utf8PathBuf>,
    pub include_glob: Option<String>,
    pub case_sensitive: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Default)]
pub struct ListTool;

#[derive(Debug, Default)]
pub struct GlobTool;

#[derive(Debug, Default)]
pub struct GrepTool;

#[async_trait(?Send)]
impl Tool for ListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::List,
            description: "List files and directories under a path",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer" },
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
        let input = serde_json::from_value::<ListInput>(raw_arguments)?;
        let requested = input.path.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded = PathGuard::require_path(ctx.workspace, &requested, AccessKind::List)?;
        ctx.confirm_if_needed(
            AccessKind::List,
            format!("List {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )?;
        if !guarded.absolute.exists() {
            return Ok(missing_directory_result(&guarded.absolute));
        }
        if guarded.absolute.is_file() {
            return Ok(file_listing_redirect_result(&guarded.absolute));
        }
        if !guarded.absolute.is_dir() {
            return Err(ToolError::Message(format!(
                "`{}` is not a directory",
                guarded.absolute
            )));
        }

        let limit = input.limit.unwrap_or(ctx.config.tool_output.max_results);
        let entries = collect_entries(
            &guarded.absolute,
            ctx.workspace,
            input.include_hidden.unwrap_or(false),
        )?;
        let mut lines = Vec::new();
        for entry in entries.iter().take(limit) {
            lines.push(entry.clone());
        }
        let output_text = lines.join("\n");
        let preview = ctx.services.truncator.preview(
            output_text,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Listed {}", guarded.absolute),
            output_text: preview.preview_text,
            metadata: json!({
                "root": guarded.absolute,
                "entry_count": entries.len(),
                "truncated": preview.truncated
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn missing_directory_result(path: &Utf8Path) -> ToolResult {
    ToolResult {
        title: format!("Directory `{path}` does not exist yet"),
        output_text: format!(
            "`{path}` does not exist yet. Do not keep retrying `list` on the same missing path. If the user already named a file under this path, create it directly with `write`; the `write` tool creates missing parent directories automatically. If you need discovery first, list the nearest existing parent directory instead."
        ),
        metadata: json!({
            "corrective_result": true,
            "missing_directory": true,
            "path": path,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

fn file_listing_redirect_result(path: &Utf8Path) -> ToolResult {
    ToolResult {
        title: format!("`{path}` is a file"),
        output_text: format!(
            "`{path}` is a file, not a directory. Use `read` to inspect its contents, or list its parent directory if you need surrounding files."
        ),
        metadata: json!({
            "corrective_result": true,
            "path_is_file": true,
            "path": path,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

#[async_trait(?Send)]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Glob,
            description: "Find files by glob pattern",
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
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
        let input = serde_json::from_value::<GlobInput>(raw_arguments)?;
        let requested = input.path.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded = PathGuard::require_path(ctx.workspace, &requested, AccessKind::Search)?;
        ctx.confirm_if_needed(
            AccessKind::Search,
            format!("Glob {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )?;

        let mut builder = GlobSetBuilder::new();
        builder.add(
            Glob::new(&input.pattern)
                .map_err(|error| ToolError::Message(format!("invalid glob pattern: {error}")))?,
        );
        let matcher = builder.build().map_err(|error| {
            ToolError::Message(format!("failed to compile glob pattern: {error}"))
        })?;
        let mut matches = collect_file_metadata(&guarded.absolute, ctx.workspace)?;
        matches.retain(|(path, _)| {
            let path = Utf8Path::new(path);
            glob_matches_path(&matcher, path, &guarded.absolute, &ctx.workspace.root)
        });
        matches.sort_by_key(|(_, modified)| Reverse(*modified));
        let limit = input.limit.unwrap_or(ctx.config.tool_output.max_results);
        let lines = matches
            .iter()
            .take(limit)
            .map(|(path, _)| glob_output_label(path, &ctx.workspace.root))
            .collect::<Vec<_>>();
        let preview = ctx.services.truncator.preview(
            lines.join("\n"),
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Glob {}", input.pattern),
            output_text: preview.preview_text,
            metadata: json!({
                "match_count": matches.len(),
                "truncated": preview.truncated
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn glob_matches_path(
    matcher: &globset::GlobSet,
    path: &Utf8Path,
    search_root: &Utf8Path,
    workspace_root: &Utf8Path,
) -> bool {
    matcher.is_match(path.as_str())
        || path
            .strip_prefix(search_root)
            .ok()
            .is_some_and(|relative| matcher.is_match(relative.as_str()))
        || path
            .strip_prefix(workspace_root)
            .ok()
            .is_some_and(|relative| matcher.is_match(relative.as_str()))
}

fn glob_output_label(path: &str, workspace_root: &Utf8Path) -> String {
    let path = Utf8Path::new(path);
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string()
}

pub(crate) fn glob_workspace_relative_pattern_fixture_passes() -> bool {
    let mut builder = GlobSetBuilder::new();
    let invariant = "workflow.glob.contract model_visible_relative_output";
    let Ok(glob) = Glob::new("src/workflow.rs") else {
        return false;
    };
    builder.add(glob);
    let Ok(matcher) = builder.build() else {
        return false;
    };
    let workspace_root = Utf8Path::new("C:/workspace/project");
    let search_root = workspace_root;
    let file_path = Utf8Path::new("C:/workspace/project/src/workflow.rs");

    glob_matches_path(&matcher, file_path, search_root, workspace_root)
        && glob_output_label(file_path.as_str(), workspace_root) == "src/workflow.rs"
        && invariant.contains("workflow.glob.contract")
        && invariant.contains("model_visible_relative_output")
}

#[async_trait(?Send)]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Grep,
            description: "Search file contents with a regex pattern",
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "include_glob": { "type": "string" },
                    "case_sensitive": { "type": "boolean" },
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
        let input = serde_json::from_value::<GrepInput>(raw_arguments)?;
        let requested = input.path.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded = PathGuard::require_path(ctx.workspace, &requested, AccessKind::Search)?;
        ctx.confirm_if_needed(
            AccessKind::Search,
            format!("Grep {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )?;

        let pattern = if input.case_sensitive.unwrap_or(false) {
            input.pattern.clone()
        } else {
            format!("(?i:{})", input.pattern)
        };
        let regex = Regex::new(&pattern)
            .map_err(|error| ToolError::Message(format!("invalid regex pattern: {error}")))?;
        let include_glob = input
            .include_glob
            .map(|value| {
                let mut builder = GlobSetBuilder::new();
                builder.add(Glob::new(&value).expect("validated by GlobSetBuilder"));
                builder.build()
            })
            .transpose()
            .map_err(|error| ToolError::Message(format!("invalid include_glob: {error}")))?;

        let mut files = collect_file_metadata(&guarded.absolute, ctx.workspace)?;
        files.sort_by_key(|(_, modified)| Reverse(*modified));
        let limit = input.limit.unwrap_or(ctx.config.tool_output.max_results);
        let mut matches = Vec::new();
        for (path, _) in files {
            if include_glob
                .as_ref()
                .map(|glob| glob.is_match(path.as_str()))
                .unwrap_or(true)
            {
                let text = match fs::read(&path) {
                    Ok(bytes) if !content_inspector::inspect(&bytes).is_binary() => {
                        String::from_utf8(bytes).ok()
                    }
                    _ => None,
                };
                if let Some(text) = text {
                    for (line_index, line) in text.lines().enumerate() {
                        if regex.is_match(line) {
                            matches.push(format!(
                                "{}:{}: {}",
                                path,
                                line_index + 1,
                                truncate_line(line)
                            ));
                            if matches.len() >= limit {
                                break;
                            }
                        }
                    }
                }
            }
            if matches.len() >= limit {
                break;
            }
        }

        let preview = ctx.services.truncator.preview(
            matches.join("\n"),
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Grep {}", input.pattern),
            output_text: preview.preview_text,
            metadata: json!({
                "total_matches": matches.len(),
                "truncated": preview.truncated
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn collect_entries(
    root: &Utf8Path,
    workspace: &crate::workspace::Workspace,
    include_hidden: bool,
) -> Result<Vec<String>, ToolError> {
    let ignore = workspace.ignore.compile()?;
    let mut builder = WalkBuilder::new(root);
    builder.hidden(!include_hidden);
    builder.git_ignore(workspace.ignore.use_gitignore);
    let mut entries = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|error| ToolError::Message(error.to_string()))?;
        let path = Utf8PathBuf::from_path_buf(entry.path().to_path_buf())
            .map_err(|_| ToolError::Message("path is not valid UTF-8".to_string()))?;
        if path == root {
            continue;
        }
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
        let label = if entry
            .file_type()
            .map(|value| value.is_dir())
            .unwrap_or(false)
        {
            format!("{}/", path.strip_prefix(root).unwrap_or(&path))
        } else {
            path.strip_prefix(root).unwrap_or(&path).to_string()
        };
        entries.push(label);
    }
    entries.sort();
    Ok(entries)
}

fn collect_file_metadata(
    root: &Utf8Path,
    workspace: &crate::workspace::Workspace,
) -> Result<Vec<(String, i64)>, ToolError> {
    let ignore = workspace.ignore.compile()?;
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.git_ignore(workspace.ignore.use_gitignore);
    let mut entries = Vec::new();
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
        let modified = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or_default();
        entries.push((path.to_string(), modified));
    }
    Ok(entries)
}

fn truncate_line(line: &str) -> String {
    const LIMIT: usize = 2_000;
    if line.len() <= LIMIT {
        line.to_string()
    } else {
        clip_text_with_ellipsis(line, LIMIT + 3)
    }
}
