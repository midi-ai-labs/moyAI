use std::fs;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde_json::json;

use crate::edit::path_for_change_storage;
use crate::error::ToolError;
use crate::session::ChangeRepository;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::write_support::{build_read_stamp, to_summary, write_text_file};
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard, is_protected_workspace_authority_path};

#[derive(Debug, Deserialize)]
pub struct WriteInput {
    pub path: Utf8PathBuf,
    pub content: String,
}

#[derive(Debug, Default)]
pub struct WriteTool;

#[async_trait(?Send)]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Write,
            description: "Create a new text file or replace the full contents of one existing text file. Prefer this for new files or clean whole-file rewrites. For existing files, read once before the first overwrite in this session. After a successful write, the written contents become the current edit baseline for later write/apply_patch calls unless another tool changes the file.",
            input_schema: json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Target file path relative to the current workspace or an allowed absolute path."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete final file contents."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<WriteInput>(raw_arguments)?;
        let guarded = PathGuard::require_path(ctx.workspace, &input.path, AccessKind::Edit, true)?;
        let mut risks = Vec::new();
        if is_protected_workspace_authority_path(&ctx.workspace.root, &guarded.absolute) {
            risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
        }
        ctx.confirm_if_needed(
            AccessKind::Edit,
            format!("Write full contents to {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            risks,
        )?;

        let services = ctx.services.clone();
        let session_id = ctx.session.session.id;
        let tool_call_id = ctx.tool_call_id;
        let path = guarded.absolute.clone();
        let stored_path = path_for_change_storage(&path, &ctx.workspace.root);
        let locked_path = path.clone();
        let path_in_task = path.clone();
        let stored_path_in_task = stored_path.clone();
        let content = input.content;
        let edit_safety = services.edit_safety.clone();
        let change = edit_safety
            .with_file_lock(&locked_path, async move {
                let original = if path_in_task.exists() {
                    let original = fs::read_to_string(&path_in_task)?;
                    let metadata = fs::metadata(&path_in_task)?;
                    let current_mtime_ms = metadata
                        .modified()
                        .ok()
                        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                        .map(|value| value.as_millis() as i64);
                    services.edit_safety.assert_fresh_write(
                        session_id,
                        &path_in_task,
                        current_mtime_ms,
                        Some(metadata.len()),
                    )?;
                    Some(original)
                } else {
                    None
                };
                if let Some(parent) = path_in_task.parent() {
                    fs::create_dir_all(parent)?;
                }
                let normalized = services.formatter.normalize_text(
                    &path_in_task,
                    original.as_deref(),
                    content,
                )?;
                let formatted = services
                    .formatter
                    .format_if_configured(&path_in_task, normalized)
                    .await?;
                if original.as_deref() == Some(formatted.as_str()) {
                    services
                        .edit_safety
                        .record_read(session_id, build_read_stamp(&path_in_task)?)?;
                    return Ok(None);
                }
                write_text_file(&path_in_task, &formatted)?;
                services
                    .edit_safety
                    .record_read(session_id, build_read_stamp(&path_in_task)?)?;
                services
                    .change_tracker
                    .build_change(
                        tool_call_id,
                        original.as_ref().map(|_| stored_path_in_task.as_ref()),
                        Some(stored_path_in_task.as_ref()),
                        original.as_deref(),
                        Some(&formatted),
                    )
                    .map(Some)
            })
            .await
            .map_err(ToolError::from)?;

        let Some(change) = change else {
            let path = guarded
                .absolute
                .strip_prefix(&ctx.workspace.root)
                .unwrap_or(guarded.absolute.as_path())
                .as_str()
                .to_string();
            return Ok(ToolResult {
                title: "No content changes made by write".to_string(),
                output_text: format!(
                    "No content changes were made to `{path}` because the formatted write content was identical to the current file. This does not satisfy an active repair operation; make a content-changing `write` or `apply_patch` before rerunning verification."
                ),
                metadata: json!({
                    "no_content_change": true,
                    "path": path,
                    "success": false,
                    "progress_effect": "no_progress",
                    "tool_feedback_envelope": {
                        "success": false,
                        "progress_effect": "no_progress",
                        "tool": "write",
                        "target": path,
                    },
                }),
                truncated_output_path: None,
                recorded_changes: Vec::new(),
                change_summaries: Vec::new(),
            });
        };
        let summary = to_summary(&change);
        let change_ids = ctx
            .services
            .store
            .change_repo()
            .insert_changes(ctx.session.session.id, &[change.clone()])
            .await?;

        Ok(ToolResult {
            title: format!("Wrote {}", summary.summary_line(Some(&ctx.workspace.root))),
            output_text: summary.tool_feedback_text(Some(&ctx.workspace.root)),
            metadata: json!({
                "changes": [json!({
                    "change_id": summary.change_id,
                    "kind": summary.kind,
                    "path_before": summary.path_before,
                    "path_after": summary.path_after
                })],
                "diff_text": change.diff_text,
            }),
            truncated_output_path: None,
            recorded_changes: change_ids,
            change_summaries: vec![summary],
        })
    }
}
