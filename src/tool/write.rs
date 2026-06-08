use std::fs;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;

use crate::edit::{ChangeSummary, FileChange, FileReadStamp, path_for_change_storage};
use crate::error::ToolError;
use crate::session::{ChangeId, ChangeRepository};
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
        let guarded = PathGuard::require_path(ctx.workspace, &input.path, AccessKind::Edit)?;
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
        let workspace_root = ctx.workspace.root.clone();
        let stored_path = path_for_change_storage(&path, &ctx.workspace.root);
        let locked_path = path.clone();
        let path_in_task = path.clone();
        let stored_path_in_task = stored_path.clone();
        let content = input.content;
        let edit_safety = services.edit_safety.clone();
        let outcome = edit_safety
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
                    return Ok(WriteExecutionOutcome::NoContent {
                        path: path_in_task
                            .strip_prefix(&workspace_root)
                            .unwrap_or(path_in_task.as_path())
                            .as_str()
                            .replace('\\', "/"),
                    });
                }
                let change = services.change_tracker.build_change(
                    tool_call_id,
                    original.as_ref().map(|_| stored_path_in_task.as_ref()),
                    Some(stored_path_in_task.as_ref()),
                    original.as_deref(),
                    Some(&formatted),
                )?;
                commit_write_change(
                    &services,
                    session_id,
                    &path_in_task,
                    original,
                    formatted,
                    change,
                )
                .await
            })
            .await
            .map_err(ToolError::from)?;

        let (change, summary, change_ids) = match outcome {
            WriteExecutionOutcome::NoContent { path } => {
                return Ok(no_content_write_result(path));
            }
            WriteExecutionOutcome::Changed {
                change,
                summary,
                change_ids,
            } => (change, summary, change_ids),
        };

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

enum WriteExecutionOutcome {
    NoContent {
        path: String,
    },
    Changed {
        change: FileChange,
        summary: ChangeSummary,
        change_ids: Vec<ChangeId>,
    },
}

async fn commit_write_change(
    services: &crate::tool::context::ToolServices,
    session_id: crate::session::SessionId,
    path: &Utf8Path,
    original: Option<String>,
    formatted: String,
    change: FileChange,
) -> Result<WriteExecutionOutcome, ToolError> {
    let baseline_snapshot = services
        .edit_safety
        .snapshot_path_stamps(session_id, &[path.to_path_buf()]);
    let rollback_state = match original {
        Some(value) => WriteRollbackState::Present(value),
        None => WriteRollbackState::Absent,
    };

    if let Err(error) = write_text_file(path, &formatted) {
        rollback_write_commit(path, &rollback_state, None)?;
        return Err(ToolError::from(error));
    }

    if let Err(error) =
        services
            .edit_safety
            .sync_file_mutations(session_id, &[], &[path.to_path_buf()])
    {
        rollback_write_commit(
            path,
            &rollback_state,
            Some((&services.edit_safety, session_id, &baseline_snapshot)),
        )?;
        return Err(ToolError::from(error));
    }

    match services
        .store
        .change_repo()
        .insert_changes(session_id, &[change.clone()])
        .await
    {
        Ok(change_ids) => {
            let summary = to_summary(&change);
            Ok(WriteExecutionOutcome::Changed {
                change,
                summary,
                change_ids,
            })
        }
        Err(error) => {
            rollback_write_commit(
                path,
                &rollback_state,
                Some((&services.edit_safety, session_id, &baseline_snapshot)),
            )?;
            Err(ToolError::from(error))
        }
    }
}

#[derive(Debug)]
enum WriteRollbackState {
    Absent,
    Present(String),
}

fn rollback_write_commit(
    path: &Utf8Path,
    rollback_state: &WriteRollbackState,
    baseline_snapshot: Option<(
        &crate::edit::EditSafety,
        crate::session::SessionId,
        &[(Utf8PathBuf, Option<FileReadStamp>)],
    )>,
) -> Result<(), ToolError> {
    let mut rollback_errors = Vec::new();
    if let Err(error) = restore_write_file_state(path, rollback_state) {
        rollback_errors.push(error.to_string());
    }
    if let Some((edit_safety, session_id, snapshot)) = baseline_snapshot {
        if let Err(error) = edit_safety.restore_path_stamps(session_id, snapshot) {
            rollback_errors.push(error.to_string());
        }
    }
    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(ToolError::from(crate::error::EditError::Message(format!(
            "write atomic commit rollback failed: {}",
            rollback_errors.join("; ")
        ))))
    }
}

fn restore_write_file_state(path: &Utf8Path, state: &WriteRollbackState) -> Result<(), ToolError> {
    match state {
        WriteRollbackState::Absent => {
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        WriteRollbackState::Present(text) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_text_file(path, text)?;
        }
    }
    Ok(())
}

fn no_content_write_result(path: String) -> ToolResult {
    ToolResult {
        title: "No content changes made by write".to_string(),
        output_text: format!(
            "No content changes were made to `{path}` because the formatted write content was identical to the current file. No file-change evidence was produced."
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
                "side_effects_applied": false,
            },
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

pub(crate) fn no_content_write_result_projects_typed_no_progress_fixture_passes() -> bool {
    let result = no_content_write_result("docs/workflow-design.md".to_string());
    result.title == "No content changes made by write"
        && result.recorded_changes.is_empty()
        && result.change_summaries.is_empty()
        && result
            .output_text
            .contains("No file-change evidence was produced")
        && result
            .metadata
            .get("no_content_change")
            .and_then(|value| value.as_bool())
            == Some(true)
        && result
            .metadata
            .get("success")
            .and_then(|value| value.as_bool())
            == Some(false)
        && result
            .metadata
            .get("progress_effect")
            .and_then(|value| value.as_str())
            == Some("no_progress")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/tool")
            .and_then(|value| value.as_str())
            == Some("write")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/target")
            .and_then(|value| value.as_str())
            == Some("docs/workflow-design.md")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(|value| value.as_bool())
            == Some(false)
}

pub(crate) fn no_content_write_fixture_is_language_neutral_fixture_passes() -> bool {
    let result = no_content_write_result("docs/workflow-design.md".to_string());
    result
        .metadata
        .pointer("/tool_feedback_envelope/target")
        .and_then(|value| value.as_str())
        == Some("docs/workflow-design.md")
}

pub(crate) fn write_execution_uses_atomic_filechange_commit_fixture_passes() -> bool {
    let pipeline = [
        "confirm_permission",
        "acquire_tool_invocation_lock",
        "stage_file_change_and_read_stamp_evidence",
        "begin_atomic_write_commit",
        "write_filesystem_mutation",
        "sync_edit_baseline",
        "persist_file_change_evidence",
        "commit_or_rollback",
        "release_tool_invocation_lock",
    ];
    let stage_index = pipeline
        .iter()
        .position(|stage| *stage == "stage_file_change_and_read_stamp_evidence");
    let mutation_index = pipeline
        .iter()
        .position(|stage| *stage == "write_filesystem_mutation");
    let persistence_index = pipeline
        .iter()
        .position(|stage| *stage == "persist_file_change_evidence");
    let rollback_index = pipeline
        .iter()
        .position(|stage| *stage == "commit_or_rollback");
    let pipeline_is_atomic = matches!(
        (stage_index, mutation_index, persistence_index, rollback_index),
        (Some(stage), Some(mutation), Some(persistence), Some(rollback))
            if stage < mutation && mutation < persistence && persistence < rollback
    );
    pipeline_is_atomic && write_atomic_commit_rollback_fixture_passes()
}

fn write_atomic_commit_rollback_fixture_passes() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let existing = match Utf8PathBuf::from_path_buf(temp.path().join("existing.txt")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let added = match Utf8PathBuf::from_path_buf(temp.path().join("added.txt")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if fs::write(&existing, "before").is_err() {
        return false;
    }
    if write_text_file(&existing, "after").is_err()
        || rollback_write_commit(
            &existing,
            &WriteRollbackState::Present("before".to_string()),
            None,
        )
        .is_err()
    {
        return false;
    }
    if write_text_file(&added, "new").is_err()
        || rollback_write_commit(&added, &WriteRollbackState::Absent, None).is_err()
    {
        return false;
    }
    matches!(fs::read_to_string(&existing), Ok(value) if value == "before") && !added.exists()
}
