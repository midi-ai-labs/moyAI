use std::fs;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;

use crate::edit::{
    ChangeSummary, FileChange, FileContentIdentity, FileReadStamp, FormatterExecutionOptions,
    path_for_change_storage,
};
use crate::error::ToolError;
use crate::session::{ChangeId, ChangeRepository};
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::write_support::{
    build_read_stamp, read_text_file_with_identity, to_summary, write_text_file,
    write_text_file_noclobber,
};
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
        ctx.run_mutation_fence.assert_owned().await?;

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
        let formatter_timeout_ms = ctx
            .config
            .shell
            .default_timeout_ms
            .min(ctx.config.shell.max_timeout_ms);
        let formatter_output_slack =
            usize::try_from(ctx.config.file_guard.max_inline_read_bytes).unwrap_or(usize::MAX);
        let formatter_cancel = ctx.cancel.clone();
        let run_mutation_fence = ctx.run_mutation_fence.clone();
        let edit_safety = services.edit_safety.clone();
        let outcome = edit_safety
            .with_file_lock(&locked_path, async move {
                let (original, expected_identity) = if path_in_task.exists() {
                    let (original, identity) = read_text_file_with_identity(&path_in_task)?;
                    services.edit_safety.assert_fresh_write(
                        session_id,
                        &path_in_task,
                        &identity,
                    )?;
                    (Some(original), Some(identity))
                } else {
                    (None, None)
                };
                let normalized = services.formatter.normalize_text(
                    &path_in_task,
                    original.as_deref(),
                    content,
                )?;
                let formatted = services
                    .formatter
                    .format_if_configured(
                        &path_in_task,
                        normalized.clone(),
                        FormatterExecutionOptions {
                            workspace_root: workspace_root.clone(),
                            timeout_ms: formatter_timeout_ms,
                            max_output_bytes: normalized
                                .len()
                                .saturating_add(formatter_output_slack),
                            cancel: formatter_cancel.clone(),
                        },
                    )
                    .await?;
                if original.as_deref() == Some(formatted.as_str()) {
                    services
                        .edit_safety
                        .assert_path_unchanged(&path_in_task, expected_identity.as_ref())?;
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
                    &run_mutation_fence,
                    session_id,
                    &path_in_task,
                    original,
                    expected_identity,
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
    run_mutation_fence: &crate::tool::context::RunMutationFence,
    session_id: crate::session::SessionId,
    path: &Utf8Path,
    original: Option<String>,
    expected_identity: Option<FileContentIdentity>,
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

    validate_write_commit_precondition(&services.edit_safety, path, expected_identity.as_ref())?;
    run_mutation_fence.assert_owned().await?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let write_result = match &rollback_state {
        WriteRollbackState::Present(_) => write_text_file(path, &formatted),
        WriteRollbackState::Absent => write_text_file_noclobber(path, &formatted),
    };
    if let Err(error) = write_result {
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

    if let Err(error) = run_mutation_fence.assert_owned().await {
        rollback_write_commit(
            path,
            &rollback_state,
            Some((&services.edit_safety, session_id, &baseline_snapshot)),
        )?;
        return Err(error);
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

fn validate_write_commit_precondition(
    edit_safety: &crate::edit::EditSafety,
    path: &Utf8Path,
    expected_identity: Option<&FileContentIdentity>,
) -> Result<(), ToolError> {
    edit_safety
        .assert_path_unchanged(path, expected_identity)
        .map_err(ToolError::from)
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

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use crate::edit::{EditSafety, read_file_with_identity};

    use super::validate_write_commit_precondition;

    #[test]
    fn commit_revalidation_preserves_same_size_external_rewrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "alpha").expect("seed file");
        let (_, expected) = read_file_with_identity(&path).expect("capture identity");

        std::fs::write(&path, "bravo").expect("external rewrite");

        validate_write_commit_precondition(&EditSafety::default(), &path, Some(&expected))
            .expect_err("external rewrite must stop the commit");
        assert_eq!(std::fs::read_to_string(&path).expect("read file"), "bravo");
    }

    #[test]
    fn commit_revalidation_preserves_externally_created_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("new.txt")).expect("utf8 path");
        std::fs::write(&path, "external").expect("external create");

        validate_write_commit_precondition(&EditSafety::default(), &path, None)
            .expect_err("external creation must stop the commit");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "external"
        );
    }
}
