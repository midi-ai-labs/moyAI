use std::fs;

use async_trait::async_trait;
use camino::Utf8Path;
use serde::Deserialize;
use serde_json::json;

use crate::config::ResolvedConfig;
use crate::edit::{
    CommittedFileMutation, FileContentIdentity, FormatterExecutionOptions, PatchOperation,
    PatchParser, ensure_edit_read_limit, path_for_change_storage,
};
use crate::error::{EditError, ToolError};
use crate::session::ChangeRepository;
use crate::tool::context::{ToolContext, ToolEffectAdmission, ToolFormatterPlan};
use crate::tool::registry::Tool;
use crate::tool::write_support::{
    MAX_EDIT_RECOVERY_PATHS, MAX_EDIT_RECOVERY_REASON_BYTES, delete_file_conditionally,
    read_text_file_with_identity, to_summary, write_text_file_conditionally,
};
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, GuardedPath, PathGuard, Workspace};

#[derive(Debug, Deserialize)]
pub struct ApplyPatchInput {
    pub patch_text: String,
}

#[derive(Debug, Default)]
pub struct ApplyPatchTool;

#[async_trait(?Send)]
impl Tool for ApplyPatchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::ApplyPatch,
            effect: crate::tool::ToolEffectPolicy::mutation(),
            description: "Apply a structured patch to one or more files using the exact `*** Begin Patch` / `*** End Patch` grammar. Read an existing file before the first `*** Update File` or `*** Delete File` operation in this session. After a successful write/apply_patch, the resulting file contents become the current edit baseline unless another tool changes the file.",
            input_schema: json!({
                "type": "object",
                "required": ["patch_text"],
                "properties": {
                    "patch_text": {
                        "type": "string",
                        "description": "Entire patch text. Must start with `*** Begin Patch` and end with `*** End Patch`. For new files, use `*** Add File: path` and prefix every added line with `+`, including blank lines and top-level code or declaration lines. Do not use unified diff markers like `---` or `+++`."
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
        let input = serde_json::from_value::<ApplyPatchInput>(raw_arguments)?;
        let operations = PatchParser::parse(&input.patch_text).map_err(ToolError::Patch)?;
        validate_apply_patch_participant_ownership(&ctx, operations.as_slice())?;
        let permission_admission =
            build_patch_permission_admission(ctx.config, ctx.workspace, operations.as_slice())?;
        let effect_admission =
            confirm_patch_permission_admission(&mut ctx, &permission_admission).await?;
        let formatter_plans = permission_admission.formatter_plans;
        let lock_paths = apply_patch_tool_invocation_lock_paths(&ctx, operations.as_slice())?;
        let edit_safety = ctx.services.edit_safety.clone();
        edit_safety
            .with_file_locks(&lock_paths, async move {
                execute_admitted_patch_operations(
                    &mut ctx,
                    operations,
                    formatter_plans,
                    effect_admission,
                )
                .await
            })
            .await
    }
}

async fn execute_admitted_patch_operations(
    ctx: &mut ToolContext<'_>,
    operations: Vec<PatchOperation>,
    formatter_plans: Vec<Option<ToolFormatterPlan>>,
    effect_admission: ToolEffectAdmission,
) -> Result<ToolResult, ToolError> {
    ctx.run_mutation_fence.assert_owned().await?;
    let admission = classify_patch_operations_before_side_effects(
        ctx,
        operations.as_slice(),
        formatter_plans.as_slice(),
        &effect_admission,
    )
    .await?;
    let all_update_operations_no_content_path = admission.all_update_operations_no_content_path;
    let first_no_content_update_path = admission.first_no_content_update_path.clone();
    let planned_operations = admission.planned_operations;
    let commit = stage_admitted_patch_commit(ctx, planned_operations)?;
    let change_ids = commit_admitted_patch(ctx, &commit).await?;

    if commit.changes.is_empty() {
        let path = all_update_operations_no_content_path
            .or(first_no_content_update_path)
            .unwrap_or_else(|| ctx.workspace.root.clone());
        return Ok(no_content_patch_result(&path, &ctx.workspace.root));
    }

    Ok(ToolResult {
        title: format!("Applied {} change(s)", commit.changes.len()),
        output_text: commit
            .summaries
            .iter()
            .map(|summary| summary.summary_line(Some(&ctx.workspace.root)))
            .collect::<Vec<_>>()
            .join("\n"),
        metadata: json!({
            "changes": commit.summaries.iter().map(|summary| json!({
                "change_id": summary.change_id,
                "kind": summary.kind,
                "path_before": summary.path_before,
                "path_after": summary.path_after
            })).collect::<Vec<_>>()
        }),
        truncated_output_path: None,
        recorded_changes: change_ids,
        change_summaries: commit.summaries.clone(),
        _internal_file_lease: None,
    })
}

fn stage_admitted_patch_commit(
    ctx: &mut ToolContext<'_>,
    planned_operations: Vec<AdmittedPatchOperation>,
) -> Result<StagedPatchCommit, ToolError> {
    let mut changes = Vec::new();
    let mut summaries = Vec::new();
    let mut mutations = Vec::new();
    let mut removed_paths = Vec::new();
    let mut current_paths = Vec::new();
    let workspace_root = ctx.workspace.root.clone();
    let tool_call_id = ctx.tool_call_id;

    for operation in planned_operations {
        let change = match operation {
            AdmittedPatchOperation::Add { path, formatted } => {
                let stored_path = path_for_apply_patch_change_storage(&path, &workspace_root);
                mutations.push(PatchMutation::Write {
                    path: path.clone(),
                    text: formatted.clone(),
                    expected_identity: None,
                    rollback: FileRollbackState::Absent,
                });
                current_paths.push(path);
                Some(
                    ctx.services
                        .change_tracker
                        .build_change(
                            tool_call_id,
                            None,
                            Some(stored_path.as_ref()),
                            None,
                            Some(&formatted),
                        )
                        .map_err(ToolError::from)?,
                )
            }
            AdmittedPatchOperation::Update {
                source_path,
                destination,
                original,
                formatted,
                source_identity,
            } => {
                let stored_source_path =
                    path_for_apply_patch_change_storage(&source_path, &workspace_root);
                let stored_destination_path =
                    path_for_apply_patch_change_storage(&destination, &workspace_root);
                mutations.push(PatchMutation::Write {
                    path: destination.clone(),
                    text: formatted.clone(),
                    expected_identity: if destination == source_path {
                        Some(source_identity.clone())
                    } else {
                        None
                    },
                    rollback: if destination == source_path {
                        FileRollbackState::Present(original.clone())
                    } else {
                        FileRollbackState::Absent
                    },
                });
                current_paths.push(destination.clone());
                if destination != source_path {
                    mutations.push(PatchMutation::Delete {
                        path: source_path.clone(),
                        expected_identity: source_identity,
                        rollback: FileRollbackState::Present(original.clone()),
                    });
                    removed_paths.push(source_path.clone());
                }
                Some(
                    ctx.services
                        .change_tracker
                        .build_change(
                            tool_call_id,
                            Some(stored_source_path.as_ref()),
                            Some(stored_destination_path.as_ref()),
                            Some(&original),
                            Some(&formatted),
                        )
                        .map_err(ToolError::from)?,
                )
            }
            AdmittedPatchOperation::Delete {
                path,
                original,
                identity,
            } => {
                let stored_path = path_for_apply_patch_change_storage(&path, &workspace_root);
                mutations.push(PatchMutation::Delete {
                    path: path.clone(),
                    expected_identity: identity,
                    rollback: FileRollbackState::Present(original.clone()),
                });
                removed_paths.push(path);
                Some(
                    ctx.services
                        .change_tracker
                        .build_change(
                            tool_call_id,
                            Some(stored_path.as_ref()),
                            None,
                            Some(&original),
                            None,
                        )
                        .map_err(ToolError::from)?,
                )
            }
            AdmittedPatchOperation::NoContent { path, identity } => {
                mutations.push(PatchMutation::NoContent {
                    path: path.clone(),
                    expected_identity: identity,
                });
                current_paths.push(path);
                None
            }
        };
        if let Some(change) = change {
            summaries.push(to_summary(&change));
            changes.push(change);
        }
    }

    Ok(StagedPatchCommit {
        changes,
        summaries,
        mutations,
        removed_paths,
        current_paths,
    })
}

async fn commit_admitted_patch(
    ctx: &mut ToolContext<'_>,
    commit: &StagedPatchCommit,
) -> Result<Vec<crate::session::ChangeId>, ToolError> {
    let session_id = ctx.session.session.id;
    let mut baseline_paths = commit.removed_paths.clone();
    baseline_paths.extend(commit.current_paths.clone());
    let baseline_snapshot = ctx
        .services
        .edit_safety
        .snapshot_path_stamps(session_id, &baseline_paths);

    ctx.run_mutation_fence.assert_owned().await?;
    let _effect_commit = ctx.run_mutation_fence.begin_effect_commit()?;
    let applied =
        match apply_patch_mutations(&ctx.services.edit_safety, &commit.mutations, ctx.workspace) {
            Ok(value) => value,
            Err((error, applied)) => {
                return Err(rollback_failed_patch_mutations(
                    error,
                    &commit.mutations,
                    &applied,
                    ctx.workspace,
                ));
            }
        };

    let committed_mutations = committed_file_mutations(&commit.mutations, &applied);

    if let Err(error) = ctx.services.edit_safety.sync_file_mutations(
        session_id,
        &committed_mutations,
        ctx.config.file_guard.max_inline_read_bytes,
    ) {
        rollback_patch_commit(
            &commit.mutations,
            &applied,
            ctx.workspace,
            Some((&ctx.services.edit_safety, session_id, &baseline_snapshot)),
        )?;
        return Err(ToolError::from(error));
    }

    if let Err(error) = ctx.run_mutation_fence.assert_owned().await {
        rollback_patch_commit(
            &commit.mutations,
            &applied,
            ctx.workspace,
            Some((&ctx.services.edit_safety, session_id, &baseline_snapshot)),
        )?;
        return Err(error);
    }

    if commit.changes.is_empty() {
        return Ok(Vec::new());
    }

    match ctx
        .services
        .store
        .change_repo()
        .insert_changes(&commit.changes)
        .await
    {
        Ok(change_ids) => Ok(change_ids),
        Err(error) => {
            rollback_patch_commit(
                &commit.mutations,
                &applied,
                ctx.workspace,
                Some((&ctx.services.edit_safety, session_id, &baseline_snapshot)),
            )?;
            Err(ToolError::from(error))
        }
    }
}

#[derive(Debug, Default)]
struct PatchOperationAdmission {
    first_no_content_update_path: Option<camino::Utf8PathBuf>,
    all_update_operations_no_content_path: Option<camino::Utf8PathBuf>,
    planned_operations: Vec<AdmittedPatchOperation>,
}

#[derive(Debug)]
enum AdmittedPatchOperation {
    Add {
        path: camino::Utf8PathBuf,
        formatted: String,
    },
    Update {
        source_path: camino::Utf8PathBuf,
        destination: camino::Utf8PathBuf,
        original: String,
        formatted: String,
        source_identity: FileContentIdentity,
    },
    Delete {
        path: camino::Utf8PathBuf,
        original: String,
        identity: FileContentIdentity,
    },
    NoContent {
        path: camino::Utf8PathBuf,
        identity: FileContentIdentity,
    },
}

#[derive(Debug)]
struct StagedPatchCommit {
    changes: Vec<crate::edit::FileChange>,
    summaries: Vec<crate::edit::ChangeSummary>,
    mutations: Vec<PatchMutation>,
    removed_paths: Vec<camino::Utf8PathBuf>,
    current_paths: Vec<camino::Utf8PathBuf>,
}

#[derive(Debug)]
enum PatchMutation {
    Write {
        path: camino::Utf8PathBuf,
        text: String,
        expected_identity: Option<FileContentIdentity>,
        rollback: FileRollbackState,
    },
    Delete {
        path: camino::Utf8PathBuf,
        expected_identity: FileContentIdentity,
        rollback: FileRollbackState,
    },
    NoContent {
        path: camino::Utf8PathBuf,
        expected_identity: FileContentIdentity,
    },
}

#[derive(Debug)]
enum FileRollbackState {
    Absent,
    Present(String),
}

#[derive(Debug)]
enum CommittedFileState {
    Absent,
    Present(FileContentIdentity),
}

#[derive(Debug)]
struct AppliedPatchMutation {
    mutation_index: usize,
    committed_state: CommittedFileState,
}

const MAX_APPLY_PATCH_PARTICIPANTS: usize = MAX_EDIT_RECOVERY_PATHS / 2;

fn committed_file_mutations(
    mutations: &[PatchMutation],
    applied: &[AppliedPatchMutation],
) -> Vec<CommittedFileMutation> {
    applied
        .iter()
        .map(|applied_mutation| {
            let path = match &mutations[applied_mutation.mutation_index] {
                PatchMutation::Write { path, .. }
                | PatchMutation::Delete { path, .. }
                | PatchMutation::NoContent { path, .. } => path.clone(),
            };
            match &applied_mutation.committed_state {
                CommittedFileState::Absent => CommittedFileMutation::absent(path),
                CommittedFileState::Present(identity) => {
                    CommittedFileMutation::present(path, identity.clone())
                }
            }
        })
        .collect()
}

fn apply_patch_mutations(
    edit_safety: &crate::edit::EditSafety,
    mutations: &[PatchMutation],
    workspace: &crate::workspace::Workspace,
) -> Result<Vec<AppliedPatchMutation>, (ToolError, Vec<AppliedPatchMutation>)> {
    if let Err(error) = validate_patch_mutation_preconditions(edit_safety, mutations) {
        return Err((error, Vec::new()));
    }
    let mut applied = Vec::with_capacity(mutations.len());
    for (mutation_index, mutation) in mutations.iter().enumerate() {
        match apply_patch_mutation(mutation, workspace) {
            Ok(committed_state) => applied.push(AppliedPatchMutation {
                mutation_index,
                committed_state,
            }),
            Err(error) => return Err((error, applied)),
        }
    }
    Ok(applied)
}

fn apply_patch_mutation(
    mutation: &PatchMutation,
    workspace: &crate::workspace::Workspace,
) -> Result<CommittedFileState, ToolError> {
    match mutation {
        PatchMutation::Write {
            path,
            text,
            expected_identity,
            ..
        } => {
            let guarded = PathGuard::require_path(workspace, path, AccessKind::Edit)?;
            PathGuard::revalidate(&guarded)?;
            let identity = write_text_file_conditionally(
                &guarded,
                text,
                expected_identity.as_ref(),
                |file| validate_patch_temporary_file(&guarded, file),
            )?;
            Ok(CommittedFileState::Present(identity))
        }
        PatchMutation::Delete {
            path,
            expected_identity,
            ..
        } => {
            let guarded = PathGuard::require_path(workspace, path, AccessKind::Edit)?;
            PathGuard::revalidate(&guarded)?;
            delete_file_conditionally(&guarded, expected_identity)?;
            Ok(CommittedFileState::Absent)
        }
        PatchMutation::NoContent {
            expected_identity, ..
        } => Ok(CommittedFileState::Present(expected_identity.clone())),
    }
}

fn validate_patch_mutation_preconditions(
    edit_safety: &crate::edit::EditSafety,
    mutations: &[PatchMutation],
) -> Result<(), ToolError> {
    for mutation in mutations {
        match mutation {
            PatchMutation::Write {
                path,
                expected_identity,
                ..
            } => edit_safety.assert_path_unchanged(path, expected_identity.as_ref())?,
            PatchMutation::Delete {
                path,
                expected_identity,
                ..
            } => edit_safety.assert_path_unchanged(path, Some(expected_identity))?,
            PatchMutation::NoContent {
                path,
                expected_identity,
            } => edit_safety.assert_path_unchanged(path, Some(expected_identity))?,
        }
    }
    Ok(())
}

fn rollback_patch_commit(
    mutations: &[PatchMutation],
    applied: &[AppliedPatchMutation],
    workspace: &crate::workspace::Workspace,
    baseline_snapshot: Option<(
        &crate::edit::EditSafety,
        crate::session::SessionId,
        &[(camino::Utf8PathBuf, Option<crate::edit::FileReadStamp>)],
    )>,
) -> Result<(), ToolError> {
    let mut rollback_error = None;
    for applied_mutation in applied.iter().rev() {
        let mutation = &mutations[applied_mutation.mutation_index];
        if let Err(error) =
            rollback_patch_mutation(mutation, &applied_mutation.committed_state, workspace)
        {
            rollback_error = Some(match rollback_error.take() {
                Some(previous) => merge_patch_failure_errors(
                    previous,
                    error,
                    PATCH_ADDITIONAL_ROLLBACK_FAILURE_LABEL,
                ),
                None => normalize_patch_rollback_error(error),
            });
        }
    }
    if rollback_error.is_none()
        && let Some((edit_safety, session_id, snapshot)) = baseline_snapshot
    {
        if let Err(error) = edit_safety.restore_path_stamps(session_id, snapshot) {
            rollback_error = Some(normalize_patch_rollback_error(ToolError::from(error)));
        }
    }
    match rollback_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

const MAX_PARTIAL_COMMIT_REASON_BYTES: usize = MAX_EDIT_RECOVERY_REASON_BYTES;
const PATCH_ROLLBACK_FAILURE_LABEL: &str = "; rollback of earlier patch mutations also failed: ";
const PATCH_ADDITIONAL_ROLLBACK_FAILURE_LABEL: &str = "; another rollback also failed: ";

fn rollback_failed_patch_mutations(
    primary_error: ToolError,
    mutations: &[PatchMutation],
    applied: &[AppliedPatchMutation],
    workspace: &crate::workspace::Workspace,
) -> ToolError {
    match rollback_patch_commit(mutations, applied, workspace, None) {
        Ok(()) => primary_error,
        Err(rollback_error) => {
            merge_patch_mutation_and_rollback_errors(primary_error, rollback_error)
        }
    }
}

fn merge_patch_mutation_and_rollback_errors(
    primary_error: ToolError,
    rollback_error: ToolError,
) -> ToolError {
    merge_patch_failure_errors(primary_error, rollback_error, PATCH_ROLLBACK_FAILURE_LABEL)
}

fn bounded_patch_failure_reason(primary: &str, label: &str, secondary: &str) -> String {
    let detail_bytes = MAX_PARTIAL_COMMIT_REASON_BYTES.saturating_sub(label.len());
    let primary_budget = detail_bytes / 2;
    let primary = truncate_utf8_bytes(primary, primary_budget);
    let secondary_budget = detail_bytes.saturating_sub(primary.len());
    let secondary = truncate_utf8_bytes(secondary, secondary_budget);
    format!("{primary}{label}{secondary}")
}

fn normalize_patch_rollback_error(error: ToolError) -> ToolError {
    if !tool_error_recovery_paths(&error).is_empty() {
        return error;
    }
    ToolError::from(EditError::RollbackFailed {
        operation: "apply_patch atomic commit".to_string(),
        details: truncate_utf8_bytes(&error.to_string(), MAX_PARTIAL_COMMIT_REASON_BYTES),
    })
}

fn merge_patch_failure_errors(
    primary_error: ToolError,
    secondary_error: ToolError,
    label: &str,
) -> ToolError {
    let mut preserved_paths = tool_error_recovery_paths(&primary_error);
    for path in tool_error_recovery_paths(&secondary_error) {
        push_unique_patch_recovery_path(&mut preserved_paths, path);
    }
    let combined_reason = bounded_patch_failure_reason(
        &primary_error.to_string(),
        label,
        &secondary_error.to_string(),
    );
    if preserved_paths.len() > 1 {
        let path = tool_error_recovery_target(&primary_error)
            .or_else(|| tool_error_recovery_target(&secondary_error))
            .expect("preserved recovery paths have a target");
        return ToolError::from(EditError::RecoveryFilesPreserved {
            path,
            preserved_paths,
            reason: combined_reason,
        });
    }
    if preserved_paths.len() == 1 {
        if !tool_error_recovery_paths(&primary_error).is_empty() {
            return replace_typed_recovery_reason(primary_error, combined_reason);
        }
        return replace_typed_recovery_reason(secondary_error, combined_reason);
    }
    ToolError::from(EditError::RollbackFailed {
        operation: "apply_patch atomic commit".to_string(),
        details: combined_reason,
    })
}

fn tool_error_recovery_paths(error: &ToolError) -> Vec<camino::Utf8PathBuf> {
    let ToolError::Edit(error) = error else {
        return Vec::new();
    };
    match error {
        EditError::CommitConflictPreserved { preserved_path, .. }
        | EditError::PartialCommit { preserved_path, .. }
        | EditError::RollbackConflictPreserved { preserved_path, .. } => {
            vec![preserved_path.clone()]
        }
        EditError::RecoveryFilesPreserved {
            preserved_paths, ..
        } => preserved_paths.clone(),
        _ => Vec::new(),
    }
}

fn tool_error_recovery_target(error: &ToolError) -> Option<camino::Utf8PathBuf> {
    let ToolError::Edit(error) = error else {
        return None;
    };
    match error {
        EditError::CommitConflictPreserved { path, .. }
        | EditError::PartialCommit { path, .. }
        | EditError::RollbackConflictPreserved { path, .. }
        | EditError::RecoveryFilesPreserved { path, .. } => Some(path.clone()),
        _ => None,
    }
}

fn replace_typed_recovery_reason(error: ToolError, reason: String) -> ToolError {
    match error {
        ToolError::Edit(EditError::CommitConflictPreserved {
            path,
            preserved_path,
            ..
        }) => ToolError::from(EditError::CommitConflictPreserved {
            path,
            preserved_path,
            reason,
        }),
        ToolError::Edit(EditError::PartialCommit {
            path,
            preserved_path,
            ..
        }) => ToolError::from(EditError::PartialCommit {
            path,
            preserved_path,
            reason,
        }),
        ToolError::Edit(EditError::RollbackConflictPreserved {
            path,
            preserved_path,
            ..
        }) => ToolError::from(EditError::RollbackConflictPreserved {
            path,
            preserved_path,
            reason,
        }),
        ToolError::Edit(EditError::RecoveryFilesPreserved {
            path,
            preserved_paths,
            ..
        }) => ToolError::from(EditError::RecoveryFilesPreserved {
            path,
            preserved_paths,
            reason,
        }),
        _ => unreachable!("typed recovery error must retain its typed recovery path"),
    }
}

fn push_unique_patch_recovery_path(
    paths: &mut Vec<camino::Utf8PathBuf>,
    path: camino::Utf8PathBuf,
) {
    if paths.contains(&path) {
        return;
    }
    assert!(
        paths.len() < MAX_EDIT_RECOVERY_PATHS,
        "bounded patch recovery path invariant exceeded"
    );
    paths.push(path);
}

fn truncate_utf8_bytes(value: &str, maximum_bytes: usize) -> String {
    const TRUNCATED: &str = "...[truncated]";
    if value.len() <= maximum_bytes {
        return value.to_string();
    }
    if maximum_bytes <= TRUNCATED.len() {
        let mut end = maximum_bytes.min(value.len());
        while !value.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        return value[..end].to_string();
    }
    let mut end = maximum_bytes.saturating_sub(TRUNCATED.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut bounded = String::with_capacity(maximum_bytes);
    bounded.push_str(&value[..end]);
    bounded.push_str(TRUNCATED);
    bounded
}

fn rollback_patch_mutation(
    mutation: &PatchMutation,
    committed_state: &CommittedFileState,
    workspace: &crate::workspace::Workspace,
) -> Result<(), ToolError> {
    match mutation {
        PatchMutation::Write { path, rollback, .. }
        | PatchMutation::Delete { path, rollback, .. } => {
            restore_file_state(path, rollback, committed_state, workspace)
        }
        PatchMutation::NoContent { .. } => Ok(()),
    }
}

fn restore_file_state(
    path: &Utf8Path,
    state: &FileRollbackState,
    committed_state: &CommittedFileState,
    workspace: &crate::workspace::Workspace,
) -> Result<(), ToolError> {
    let guarded = PathGuard::require_path(workspace, path, AccessKind::Edit)?;
    PathGuard::revalidate(&guarded)?;
    let unchanged = match committed_state {
        CommittedFileState::Present(identity) => crate::edit::EditSafety::default()
            .assert_path_unchanged(path, Some(identity))
            .is_ok(),
        CommittedFileState::Absent => matches!(
            fs::symlink_metadata(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ),
    };
    if !unchanged {
        return Err(ToolError::from(crate::error::EditError::RollbackConflict {
            path: path.to_path_buf(),
        }));
    }
    match state {
        FileRollbackState::Absent => {
            let CommittedFileState::Present(identity) = committed_state else {
                return Ok(());
            };
            delete_file_conditionally(&guarded, identity)
                .map_err(|error| rollback_conflict(path, error))?;
        }
        FileRollbackState::Present(text) => {
            let expected_identity = match committed_state {
                CommittedFileState::Present(identity) => Some(identity),
                CommittedFileState::Absent => None,
            };
            write_text_file_conditionally(&guarded, text, expected_identity, |file| {
                validate_patch_temporary_file(&guarded, file)
            })
            .map_err(|error| rollback_conflict(path, error))?;
        }
    }
    Ok(())
}

fn rollback_conflict(path: &Utf8Path, error: crate::error::EditError) -> ToolError {
    match error {
        crate::error::EditError::CommitConflictPreserved {
            preserved_path,
            reason,
            ..
        } => ToolError::from(crate::error::EditError::RollbackConflictPreserved {
            path: path.to_path_buf(),
            preserved_path,
            reason,
        }),
        crate::error::EditError::CommitConflict { .. } => {
            ToolError::from(crate::error::EditError::RollbackConflict {
                path: path.to_path_buf(),
            })
        }
        crate::error::EditError::PartialCommit {
            preserved_path,
            reason,
            ..
        } => ToolError::from(crate::error::EditError::RollbackConflictPreserved {
            path: path.to_path_buf(),
            preserved_path,
            reason,
        }),
        other => ToolError::from(other),
    }
}

fn validate_patch_temporary_file(
    guarded: &GuardedPath,
    file: &std::fs::File,
) -> Result<(), crate::error::EditError> {
    PathGuard::validate_open_file_within_boundary(guarded, file)
        .map_err(|error| crate::error::EditError::Message(error.to_string()))
}

#[derive(Debug)]
struct PatchPermissionAdmission {
    access: AccessKind,
    targets: Vec<camino::Utf8PathBuf>,
    outside_workspace: bool,
    risks: Vec<PermissionRisk>,
    details: Vec<String>,
    formatter_plans: Vec<Option<ToolFormatterPlan>>,
}

impl Default for PatchPermissionAdmission {
    fn default() -> Self {
        Self {
            access: AccessKind::Edit,
            targets: Vec::new(),
            outside_workspace: false,
            risks: Vec::new(),
            details: Vec::new(),
            formatter_plans: Vec::new(),
        }
    }
}

async fn classify_patch_operations_before_side_effects(
    ctx: &ToolContext<'_>,
    operations: &[PatchOperation],
    formatter_plans: &[Option<ToolFormatterPlan>],
    effect_admission: &ToolEffectAdmission,
) -> Result<PatchOperationAdmission, ToolError> {
    if operations.len() != formatter_plans.len() {
        return Err(ToolError::Message(
            "apply_patch formatter plan count changed after permission admission".to_string(),
        ));
    }
    let mut saw_update = false;
    let mut saw_non_update = false;
    let mut saw_content_changing_update = false;
    let mut no_content_path = None;
    let mut planned_operations = Vec::new();
    for (operation_index, operation) in operations.iter().enumerate() {
        let formatter_plan = formatter_plans[operation_index].as_ref();
        match operation {
            PatchOperation::Add { path, contents } => {
                debug_assert!(patch_operation_requires_pre_side_effect_admission(
                    operation
                ));
                saw_non_update = true;
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                validate_patch_formatter_plan_target(
                    formatter_plan,
                    Some(&guarded.absolute),
                    "add",
                )?;
                if guarded.absolute.exists() {
                    return Err(ToolError::from(crate::error::EditError::Message(
                        add_existing_path_message(&guarded.absolute),
                    )));
                }
                let normalized = ctx.services.formatter.normalize_text(
                    &ctx.config.format,
                    &guarded.absolute,
                    None,
                    contents.clone(),
                )?;
                let formatted = effect_admission
                    .format_if_planned(
                        &ctx.services.formatter,
                        formatter_plan,
                        normalized.clone(),
                        formatter_execution_options(ctx),
                    )
                    .await?;
                planned_operations.push(AdmittedPatchOperation::Add {
                    path: guarded.absolute,
                    formatted,
                });
            }
            PatchOperation::Update {
                path,
                move_to,
                hunks,
            } => {
                debug_assert!(patch_operation_requires_pre_side_effect_admission(
                    operation
                ));
                saw_update = true;
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                let move_guard = move_to
                    .as_ref()
                    .map(|target| PathGuard::require_path(ctx.workspace, target, AccessKind::Edit))
                    .transpose()?;
                let source_path = guarded.absolute.clone();
                let destination = move_guard
                    .as_ref()
                    .map(|target| target.absolute.clone())
                    .unwrap_or_else(|| source_path.clone());
                validate_patch_formatter_plan_target(formatter_plan, Some(&destination), "update")?;
                validate_move_destination_admission(&source_path, &destination)?;
                let (original, source_identity) = read_text_file_with_identity(
                    &guarded,
                    ctx.config.file_guard.max_inline_read_bytes,
                )?;
                ctx.services.edit_safety.assert_fresh_write(
                    ctx.session.session.id,
                    &source_path,
                    &source_identity,
                )?;
                let patched = PatchParser::apply_to_text(&original, hunks)
                    .map_err(|error| crate::error::EditError::Message(error.to_string()))?;
                let normalized = ctx.services.formatter.normalize_text(
                    &ctx.config.format,
                    &destination,
                    Some(&original),
                    patched,
                )?;
                let formatted = effect_admission
                    .format_if_planned(
                        &ctx.services.formatter,
                        formatter_plan,
                        normalized.clone(),
                        formatter_execution_options(ctx),
                    )
                    .await?;
                if update_operation_is_no_content(&formatted, &original, &destination, &source_path)
                {
                    no_content_path.get_or_insert_with(|| destination.clone());
                    planned_operations.push(AdmittedPatchOperation::NoContent {
                        path: destination,
                        identity: source_identity,
                    });
                    continue;
                }
                saw_content_changing_update = true;
                planned_operations.push(AdmittedPatchOperation::Update {
                    source_path,
                    destination,
                    original,
                    formatted,
                    source_identity,
                });
            }
            PatchOperation::Delete { path } => {
                debug_assert!(patch_operation_requires_pre_side_effect_admission(
                    operation
                ));
                saw_non_update = true;
                validate_patch_formatter_plan_target(formatter_plan, None, "delete")?;
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                let (original, identity) = read_text_file_with_identity(
                    &guarded,
                    ctx.config.file_guard.max_inline_read_bytes,
                )?;
                ctx.services.edit_safety.assert_fresh_write(
                    ctx.session.session.id,
                    &guarded.absolute,
                    &identity,
                )?;
                planned_operations.push(AdmittedPatchOperation::Delete {
                    path: guarded.absolute,
                    original,
                    identity,
                });
            }
        }
    }
    let all_update_operations_no_content_path = all_update_operations_no_content_path(
        saw_update,
        saw_non_update,
        saw_content_changing_update,
        no_content_path.clone(),
    );
    validate_admitted_patch_final_content_limits(
        planned_operations.as_slice(),
        ctx.config.file_guard.max_inline_read_bytes,
    )?;
    Ok(PatchOperationAdmission {
        first_no_content_update_path: no_content_path,
        all_update_operations_no_content_path,
        planned_operations,
    })
}

fn validate_patch_formatter_plan_target(
    formatter_plan: Option<&ToolFormatterPlan>,
    expected_target: Option<&Utf8Path>,
    operation: &str,
) -> Result<(), ToolError> {
    match (formatter_plan, expected_target) {
        (Some(plan), Some(expected_target)) if plan.target() == expected_target => Ok(()),
        (None, Some(_)) | (None, None) => Ok(()),
        (Some(_), None) => Err(ToolError::Message(format!(
            "apply_patch {operation} operation unexpectedly retained a formatter plan"
        ))),
        (Some(plan), Some(expected_target)) => Err(ToolError::Message(format!(
            "apply_patch {operation} formatter plan targets `{}` instead of `{expected_target}`",
            plan.target()
        ))),
    }
}

fn validate_admitted_patch_final_content_limits(
    planned_operations: &[AdmittedPatchOperation],
    maximum_bytes: u64,
) -> Result<(), ToolError> {
    for operation in planned_operations {
        let (path, formatted) = match operation {
            AdmittedPatchOperation::Add { path, formatted }
            | AdmittedPatchOperation::Update {
                destination: path,
                formatted,
                ..
            } => (path, formatted),
            AdmittedPatchOperation::Delete { .. } | AdmittedPatchOperation::NoContent { .. } => {
                continue;
            }
        };
        ensure_edit_read_limit(
            path,
            u64::try_from(formatted.len()).unwrap_or(u64::MAX),
            maximum_bytes,
        )?;
    }
    Ok(())
}

fn patch_operation_requires_pre_side_effect_admission(operation: &PatchOperation) -> bool {
    matches!(
        operation,
        PatchOperation::Add { .. } | PatchOperation::Update { .. } | PatchOperation::Delete { .. }
    )
}

fn formatter_execution_options(ctx: &ToolContext<'_>) -> FormatterExecutionOptions {
    FormatterExecutionOptions {
        timeout_ms: ctx
            .config
            .shell
            .default_timeout_ms
            .min(ctx.config.shell.max_timeout_ms),
        max_output_bytes: usize::try_from(ctx.config.file_guard.max_inline_read_bytes)
            .unwrap_or(usize::MAX),
        cancel: ctx.cancel.clone(),
    }
}

fn build_patch_permission_admission(
    config: &ResolvedConfig,
    workspace: &Workspace,
    operations: &[PatchOperation],
) -> Result<PatchPermissionAdmission, ToolError> {
    let mut admission = PatchPermissionAdmission::default();
    for operation in operations {
        match operation {
            PatchOperation::Add { path, .. } => {
                let guarded = PathGuard::require_path(workspace, path, AccessKind::Edit)?;
                extend_patch_permission_admission(
                    &mut admission,
                    workspace.root.as_path(),
                    &guarded,
                    None,
                    false,
                    false,
                    "add",
                );
                let formatter_plan = ToolFormatterPlan::resolve(config, workspace, &guarded)?;
                push_patch_formatter_plan(&mut admission, formatter_plan);
            }
            PatchOperation::Update { path, move_to, .. } => {
                let guarded = PathGuard::require_path(workspace, path, AccessKind::Edit)?;
                let move_guard = move_to
                    .as_ref()
                    .map(|target| PathGuard::require_path(workspace, target, AccessKind::Edit))
                    .transpose()?;
                let move_or_rename = move_guard
                    .as_ref()
                    .is_some_and(|target| target.absolute != guarded.absolute);
                extend_patch_permission_admission(
                    &mut admission,
                    workspace.root.as_path(),
                    &guarded,
                    move_guard.as_ref(),
                    false,
                    move_or_rename,
                    if move_or_rename {
                        "move/update"
                    } else {
                        "update"
                    },
                );
                let formatter_target = move_guard.as_ref().unwrap_or(&guarded);
                let formatter_plan =
                    ToolFormatterPlan::resolve(config, workspace, formatter_target)?;
                push_patch_formatter_plan(&mut admission, formatter_plan);
            }
            PatchOperation::Delete { path } => {
                let guarded = PathGuard::require_path(workspace, path, AccessKind::Edit)?;
                extend_patch_permission_admission(
                    &mut admission,
                    workspace.root.as_path(),
                    &guarded,
                    None,
                    true,
                    false,
                    "delete",
                );
                push_patch_formatter_plan(&mut admission, None);
            }
        }
    }
    if admission.formatter_plans.len() != operations.len() {
        return Err(ToolError::Message(
            "apply_patch formatter plan count did not match parsed operations".to_string(),
        ));
    }
    Ok(admission)
}

fn push_patch_formatter_plan(
    admission: &mut PatchPermissionAdmission,
    formatter_plan: Option<ToolFormatterPlan>,
) {
    if let Some(plan) = formatter_plan.as_ref() {
        admission.access = AccessKind::Shell;
        admission.details.push(plan.permission_detail());
    }
    admission.formatter_plans.push(formatter_plan);
}

fn apply_patch_tool_invocation_lock_paths(
    ctx: &ToolContext<'_>,
    operations: &[PatchOperation],
) -> Result<Vec<camino::Utf8PathBuf>, ToolError> {
    let mut paths = Vec::new();
    for operation in operations {
        match operation {
            PatchOperation::Add { path, .. } | PatchOperation::Delete { path } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                paths.push(guarded.absolute);
            }
            PatchOperation::Update { path, move_to, .. } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                paths.push(guarded.absolute);
                if let Some(target) = move_to {
                    let target = PathGuard::require_path(ctx.workspace, target, AccessKind::Edit)?;
                    paths.push(target.absolute);
                }
            }
        }
    }
    Ok(normalize_apply_patch_tool_invocation_lock_paths(paths))
}

fn normalize_apply_patch_tool_invocation_lock_paths(
    mut paths: Vec<camino::Utf8PathBuf>,
) -> Vec<camino::Utf8PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

fn extend_patch_permission_admission(
    admission: &mut PatchPermissionAdmission,
    workspace_root: &Utf8Path,
    guarded: &GuardedPath,
    move_guard: Option<&GuardedPath>,
    destructive_delete: bool,
    move_or_rename: bool,
    operation_name: &str,
) {
    push_unique_path(&mut admission.targets, guarded.absolute.clone());
    if let Some(target) = move_guard {
        push_unique_path(&mut admission.targets, target.absolute.clone());
    }
    admission.outside_workspace |= edit_request_is_outside_workspace(guarded, move_guard);
    for risk in edit_request_risks(
        workspace_root,
        guarded,
        move_guard,
        destructive_delete,
        move_or_rename,
    ) {
        push_unique_risk(&mut admission.risks, risk);
    }
    admission
        .details
        .push(patch_permission_detail(operation_name, guarded, move_guard));
}

async fn confirm_patch_permission_admission(
    ctx: &mut ToolContext<'_>,
    admission: &PatchPermissionAdmission,
) -> Result<ToolEffectAdmission, ToolError> {
    ctx.confirm_if_needed_with_details(
        admission.access,
        format!(
            "Apply patch as one tool invocation to {} target(s)",
            admission.targets.len()
        ),
        admission.details.clone(),
        admission.targets.clone(),
        admission.outside_workspace,
        admission.risks.clone(),
    )
    .await
}

fn patch_permission_detail(
    operation_name: &str,
    guarded: &GuardedPath,
    move_guard: Option<&GuardedPath>,
) -> String {
    if let Some(target) = move_guard {
        format!(
            "{operation_name}: {} -> {}",
            guarded.absolute, target.absolute
        )
    } else {
        format!("{operation_name}: {}", guarded.absolute)
    }
}

fn push_unique_path(paths: &mut Vec<camino::Utf8PathBuf>, path: camino::Utf8PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn push_unique_risk(risks: &mut Vec<PermissionRisk>, risk: PermissionRisk) {
    if !risks.contains(&risk) {
        risks.push(risk);
    }
}

fn update_operation_is_no_content(
    formatted: &str,
    original: &str,
    destination: &Utf8Path,
    source_path: &Utf8Path,
) -> bool {
    formatted == original && destination == source_path
}

fn all_update_operations_no_content_path(
    saw_update: bool,
    saw_non_update: bool,
    saw_content_changing_update: bool,
    first_no_content_path: Option<camino::Utf8PathBuf>,
) -> Option<camino::Utf8PathBuf> {
    if saw_update && !saw_non_update && !saw_content_changing_update {
        first_no_content_path
    } else {
        None
    }
}

fn validate_apply_patch_participant_ownership(
    ctx: &ToolContext<'_>,
    operations: &[PatchOperation],
) -> Result<(), ToolError> {
    let mut owned_participants = Vec::new();
    for operation in operations {
        match operation {
            PatchOperation::Add { path, .. } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                claim_apply_patch_participant(
                    &mut owned_participants,
                    guarded.absolute.as_path(),
                    "add",
                )?;
            }
            PatchOperation::Update { path, move_to, .. } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                claim_apply_patch_participant(
                    &mut owned_participants,
                    guarded.absolute.as_path(),
                    "update source",
                )?;
                if let Some(target) = move_to {
                    let target = PathGuard::require_path(ctx.workspace, target, AccessKind::Edit)?;
                    if target.absolute != guarded.absolute {
                        claim_apply_patch_participant(
                            &mut owned_participants,
                            target.absolute.as_path(),
                            "update destination",
                        )?;
                    }
                }
            }
            PatchOperation::Delete { path } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                claim_apply_patch_participant(
                    &mut owned_participants,
                    guarded.absolute.as_path(),
                    "delete",
                )?;
            }
        }
    }
    Ok(())
}

fn claim_apply_patch_participant(
    owned_participants: &mut Vec<camino::Utf8PathBuf>,
    path: &Utf8Path,
    operation_name: &str,
) -> Result<(), ToolError> {
    if owned_participants.iter().any(|owned| owned == path) {
        return Err(ToolError::from(crate::error::EditError::Message(format!(
            "path `{path}` has multiple content-changing owners in one apply_patch invocation ({operation_name})"
        ))));
    }
    if owned_participants.len() >= MAX_APPLY_PATCH_PARTICIPANTS {
        return Err(ToolError::from(crate::error::EditError::Message(format!(
            "apply_patch has more than {MAX_APPLY_PATCH_PARTICIPANTS} file participants; split the patch before any mutation"
        ))));
    }
    owned_participants.push(path.to_path_buf());
    Ok(())
}

fn validate_move_destination_admission(
    source_path: &Utf8Path,
    destination: &Utf8Path,
) -> Result<(), crate::error::EditError> {
    if destination != source_path && destination.exists() {
        return Err(crate::error::EditError::Message(format!(
            "move destination `{destination}` already exists for source `{source_path}`"
        )));
    }
    Ok(())
}

fn add_existing_path_message(path: &Utf8Path) -> String {
    format!("path `{path}` already exists")
}

fn no_content_patch_message(path: &Utf8Path) -> String {
    format!("apply_patch made no content changes to `{path}`")
}

fn no_content_patch_result(path: &Utf8Path, workspace_root: &Utf8Path) -> ToolResult {
    let display_path = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .as_str()
        .replace('\\', "/");
    ToolResult {
        title: "No content changes made by apply_patch".to_string(),
        output_text: no_content_patch_message(Utf8Path::new(&display_path)),
        metadata: json!({
            "no_content_change": true,
            "path": display_path.clone(),
            "success": false
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    }
}

fn path_for_apply_patch_change_storage(
    path: &Utf8Path,
    workspace_root: &Utf8Path,
) -> camino::Utf8PathBuf {
    path_for_change_storage(path, workspace_root)
}

fn edit_request_is_outside_workspace(
    guarded: &GuardedPath,
    move_guard: Option<&GuardedPath>,
) -> bool {
    (!guarded.inside_workspace && !guarded.trusted_external)
        || move_guard.is_some_and(|target| !target.inside_workspace && !target.trusted_external)
}

fn edit_request_risks(
    workspace_root: &Utf8Path,
    guarded: &GuardedPath,
    move_guard: Option<&GuardedPath>,
    destructive_delete: bool,
    move_or_rename: bool,
) -> Vec<PermissionRisk> {
    let mut risks = Vec::new();
    if destructive_delete {
        risks.push(PermissionRisk::DestructiveDelete);
    }
    if move_or_rename {
        risks.push(PermissionRisk::MoveOrRename);
    }
    if PathGuard::targets_protected_workspace_authority(workspace_root, guarded)
        || move_guard.as_ref().is_some_and(|target| {
            PathGuard::targets_protected_workspace_authority(workspace_root, target)
        })
    {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    risks
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use std::sync::{Arc, Barrier};

    use crate::config::{AccessMode, FormatConfig, FormatterRule, NewlineStyle};
    use crate::edit::{
        CommittedFileMutation, EditSafety, Formatter, FormatterExecutionOptions, PatchOperation,
        read_file_with_identity,
    };
    use crate::protocol::TurnInterruptionCause;
    use crate::runtime::{RunCancelOutcome, RunCancellationCause, RunControl};
    use crate::session::SessionId;
    use crate::tool::context::{
        ToolEffectAdmission, ToolFormatterPlan, access_mode_allows_permission,
    };
    use crate::workspace::{AccessKind, PathGuard, Workspace, WorkspaceDiscovery};

    use super::{
        CommittedFileState, FileRollbackState, PatchMutation, apply_patch_mutations,
        build_patch_permission_admission, committed_file_mutations, restore_file_state,
        validate_patch_formatter_plan_target,
    };

    fn test_workspace(root: &camino::Utf8Path) -> Workspace {
        WorkspaceDiscovery::discover_fixed_root(root, &crate::config::ResolvedConfig::default())
            .expect("test workspace")
    }

    fn marker_format_config() -> FormatConfig {
        FormatConfig {
            default_newline: NewlineStyle::Lf,
            ensure_trailing_newline: true,
            commands: vec![FormatterRule {
                glob: "**/*.txt".to_string(),
                command: marker_command(),
            }],
        }
    }

    fn formatter_plan(
        config: &crate::config::ResolvedConfig,
        workspace: &Workspace,
        target: &camino::Utf8Path,
    ) -> ToolFormatterPlan {
        let guarded = PathGuard::require_path(workspace, target, AccessKind::Edit)
            .expect("guard formatter target");
        ToolFormatterPlan::resolve(config, workspace, &guarded)
            .expect("resolve formatter plan")
            .expect("matching formatter plan")
    }

    fn formatter_options(control: &RunControl) -> FormatterExecutionOptions {
        FormatterExecutionOptions {
            timeout_ms: 5_000,
            max_output_bytes: 1_024,
            cancel: control.token(),
        }
    }

    #[cfg(windows)]
    fn marker_command() -> Vec<String> {
        vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "Set-Content -LiteralPath 'formatter-started.marker' -Value 'started'; [Console]::In.ReadToEnd()"
                .to_string(),
        ]
    }

    #[test]
    fn any_matching_formatter_promotes_the_combined_patch_permission_to_shell() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut config = crate::config::ResolvedConfig::default();
        config.format = marker_format_config();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let operations = vec![PatchOperation::Add {
            path: Utf8PathBuf::from("output.txt"),
            contents: "content".to_string(),
        }];

        let admission = build_patch_permission_admission(&config, &workspace, &operations)
            .expect("build permission admission");
        let request = crate::tool::PermissionRequest {
            access: admission.access,
            summary: "patch with configured formatter".to_string(),
            details: admission.details.clone(),
            targets: admission.targets.clone(),
            outside_workspace: admission.outside_workspace,
            risks: admission.risks.clone(),
            agent_path: None,
            agent_task_name: None,
        };

        assert_eq!(request.access, AccessKind::Shell);
        assert_eq!(admission.formatter_plans.len(), operations.len());
        assert!(admission.formatter_plans[0].is_some());
        let formatter_plan = admission.formatter_plans[0]
            .as_ref()
            .expect("matching formatter plan");
        validate_patch_formatter_plan_target(
            Some(formatter_plan),
            Some(&root.join("different.txt")),
            "add",
        )
        .expect_err("operation and formatter plan targets must remain aligned");
        validate_patch_formatter_plan_target(Some(formatter_plan), None, "delete")
            .expect_err("delete operations cannot retain formatter plans");
        assert!(!access_mode_allows_permission(
            AccessMode::Default,
            &request
        ));
        assert!(!access_mode_allows_permission(
            AccessMode::FullAccess,
            &request
        ));
        assert!(
            request
                .details
                .iter()
                .any(|detail| detail.contains("argv="))
        );

        config.format.commands[0].glob = "**/*.rs".to_string();
        let no_match = build_patch_permission_admission(&config, &workspace, &operations)
            .expect("build non-formatter permission admission");
        assert_eq!(no_match.access, AccessKind::Edit);
        assert!(no_match.formatter_plans[0].is_none());
    }

    #[cfg(not(windows))]
    fn marker_command() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "touch formatter-started.marker; cat".to_string(),
        ]
    }

    #[tokio::test]
    async fn later_patch_formatter_does_not_spawn_after_typed_terminal() {
        for cause in [
            RunCancellationCause::Interruption(TurnInterruptionCause::UserStop),
            RunCancellationCause::Failure("provider failed between formatters".to_string()),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root =
                Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 workspace");
            let first_dir = root.join("first");
            let second_dir = root.join("second");
            std::fs::create_dir_all(&first_dir).expect("first formatter directory");
            std::fs::create_dir_all(&second_dir).expect("second formatter directory");
            let first_path = first_dir.join("first.txt");
            let second_path = second_dir.join("second.txt");
            let control = RunControl::new();
            let admission = ToolEffectAdmission::new(control.clone());
            let format = marker_format_config();
            let mut config = crate::config::ResolvedConfig::default();
            config.format = format.clone();
            let workspace = test_workspace(&root);
            let first_plan = formatter_plan(&config, &workspace, &first_path);
            let second_plan = formatter_plan(&config, &workspace, &second_path);
            let formatter = Formatter::new(format);

            admission
                .format_if_planned(
                    &formatter,
                    Some(&first_plan),
                    "first".to_string(),
                    formatter_options(&control),
                )
                .await
                .expect("first formatter effect");
            assert!(first_dir.join("formatter-started.marker").exists());
            assert_eq!(control.request_cancel(cause), RunCancelOutcome::Applied);

            let error = admission
                .format_if_planned(
                    &formatter,
                    Some(&second_plan),
                    "second".to_string(),
                    formatter_options(&control),
                )
                .await
                .expect_err("terminal owner must block the later formatter");
            assert!(matches!(error, crate::error::ToolError::RunInterrupted));
            assert!(!second_dir.join("formatter-started.marker").exists());
            assert!(!first_path.exists());
            assert!(!second_path.exists());
        }
    }

    #[test]
    fn stop_before_apply_patch_admission_has_zero_file_mutations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("must-not-exist.txt")).expect("utf8 path");
        let workspace = test_workspace(path.parent().expect("parent"));
        let control = RunControl::new();
        let effect_admission = ToolEffectAdmission::new(control.clone());
        let ready = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_ready = Arc::clone(&ready);
        let worker_release = Arc::clone(&release);
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            let mutations = vec![PatchMutation::Write {
                path: worker_path,
                text: "must not be written".to_string(),
                expected_identity: None,
                rollback: FileRollbackState::Absent,
            }];
            worker_ready.wait();
            worker_release.wait();
            effect_admission
                .admit()
                .map_err(|error| (error, Vec::new()))?;
            apply_patch_mutations(&EditSafety::default(), &mutations, &workspace)
        });

        ready.wait();
        assert_eq!(
            control.request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            )),
            RunCancelOutcome::Applied
        );
        release.wait();
        let (error, applied) = worker
            .join()
            .expect("patch worker")
            .expect_err("Stop must win before patch effects");

        assert!(matches!(error, crate::error::ToolError::RunInterrupted));
        assert_eq!(applied.len(), 0);
        assert!(!path.exists());
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
    }

    #[test]
    fn mutation_commit_preserves_same_size_external_rewrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        let workspace = test_workspace(path.parent().expect("parent"));
        std::fs::write(&path, "alpha").expect("seed file");
        let (_, expected_identity) =
            read_file_with_identity(&path, 1_024).expect("capture identity");
        let mutations = vec![PatchMutation::Write {
            path: path.clone(),
            text: "agent".to_string(),
            expected_identity: Some(expected_identity),
            rollback: FileRollbackState::Present("alpha".to_string()),
        }];

        std::fs::write(&path, "bravo").expect("external rewrite");

        let (_, applied) = apply_patch_mutations(&EditSafety::default(), &mutations, &workspace)
            .expect_err("external rewrite must stop the commit");
        assert_eq!(applied.len(), 0);
        assert_eq!(std::fs::read_to_string(&path).expect("read file"), "bravo");
    }

    #[test]
    fn mutation_commit_preserves_externally_created_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("new.txt")).expect("utf8 path");
        let workspace = test_workspace(path.parent().expect("parent"));
        let mutations = vec![PatchMutation::Write {
            path: path.clone(),
            text: "agent".to_string(),
            expected_identity: None,
            rollback: FileRollbackState::Absent,
        }];
        std::fs::write(&path, "external").expect("external create");

        let (_, applied) = apply_patch_mutations(&EditSafety::default(), &mutations, &workspace)
            .expect_err("external creation must stop the commit");
        assert_eq!(applied.len(), 0);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "external"
        );
    }

    #[test]
    fn all_preconditions_are_checked_before_first_multi_file_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = Utf8PathBuf::from_path_buf(temp.path().join("first.txt")).expect("utf8 path");
        let second = Utf8PathBuf::from_path_buf(temp.path().join("second.txt")).expect("utf8 path");
        let workspace = test_workspace(first.parent().expect("parent"));
        std::fs::write(&first, "first-old").expect("seed first");
        std::fs::write(&second, "second-old").expect("seed second");
        let (_, first_identity) = read_file_with_identity(&first, 1_024).expect("first identity");
        let (_, second_identity) =
            read_file_with_identity(&second, 1_024).expect("second identity");
        let mutations = vec![
            PatchMutation::Write {
                path: first.clone(),
                text: "first-agent".to_string(),
                expected_identity: Some(first_identity),
                rollback: FileRollbackState::Present("first-old".to_string()),
            },
            PatchMutation::Write {
                path: second.clone(),
                text: "second-agent".to_string(),
                expected_identity: Some(second_identity),
                rollback: FileRollbackState::Present("second-old".to_string()),
            },
        ];
        std::fs::write(&second, "second-out").expect("external rewrite");

        let (_, applied) = apply_patch_mutations(&EditSafety::default(), &mutations, &workspace)
            .expect_err("any stale participant must stop the whole commit");
        assert_eq!(applied.len(), 0);
        assert_eq!(
            std::fs::read_to_string(&first).expect("read first"),
            "first-old"
        );
        assert_eq!(
            std::fs::read_to_string(&second).expect("read second"),
            "second-out"
        );
    }

    #[test]
    fn stale_no_content_participant_stops_before_first_file_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let added = Utf8PathBuf::from_path_buf(temp.path().join("added.txt")).expect("utf8 path");
        let unchanged =
            Utf8PathBuf::from_path_buf(temp.path().join("unchanged.txt")).expect("utf8 path");
        let workspace = test_workspace(added.parent().expect("parent"));
        std::fs::write(&unchanged, "before").expect("seed unchanged file");
        let (_, expected_identity) =
            read_file_with_identity(&unchanged, 1_024).expect("unchanged identity");
        let mutations = vec![
            PatchMutation::Write {
                path: added.clone(),
                text: "agent".to_string(),
                expected_identity: None,
                rollback: FileRollbackState::Absent,
            },
            PatchMutation::NoContent {
                path: unchanged.clone(),
                expected_identity,
            },
        ];
        std::fs::write(&unchanged, "extern").expect("external rewrite");

        let (_, applied) = apply_patch_mutations(&EditSafety::default(), &mutations, &workspace)
            .expect_err("stale no-content participant must stop the whole commit");

        assert!(applied.is_empty());
        assert!(!added.exists());
        assert_eq!(
            std::fs::read_to_string(&unchanged).expect("read unchanged participant"),
            "extern"
        );
    }

    #[test]
    fn no_content_participant_is_retained_for_exact_post_commit_sync() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("unchanged.txt")).expect("utf8 path");
        let workspace = test_workspace(path.parent().expect("parent"));
        std::fs::write(&path, "before").expect("seed unchanged file");
        let (_, expected_identity) =
            read_file_with_identity(&path, 1_024).expect("unchanged identity");
        let mutations = vec![PatchMutation::NoContent {
            path: path.clone(),
            expected_identity: expected_identity.clone(),
        }];

        let applied = apply_patch_mutations(&EditSafety::default(), &mutations, &workspace)
            .expect("no-content precondition");
        let committed = committed_file_mutations(&mutations, &applied);
        assert_eq!(
            committed,
            vec![CommittedFileMutation::Present {
                path: path.clone(),
                identity: expected_identity,
            }]
        );

        std::fs::write(&path, "extern").expect("post-precondition external rewrite");
        EditSafety::default()
            .sync_file_mutations(SessionId::new(), &committed, 1_024)
            .expect_err("post-commit sync must reject a replaced no-content participant");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read external rewrite"),
            "extern"
        );
    }

    #[test]
    fn rollback_cas_preserves_external_rewrite_after_patch_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        let workspace = test_workspace(path.parent().expect("parent"));
        std::fs::write(&path, "agent").expect("agent write");
        let (_, identity) = read_file_with_identity(&path, 1_024).expect("agent identity");
        std::fs::write(&path, "external").expect("external rewrite");

        let error = restore_file_state(
            &path,
            &FileRollbackState::Present("old".to_string()),
            &CommittedFileState::Present(identity),
            &workspace,
        )
        .expect_err("rollback conflict must be explicit");

        assert!(error.to_string().contains("partially committed"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "external"
        );
    }

    #[test]
    fn partial_commit_merge_keeps_recovery_identity_and_bounds_rollback_detail() {
        let path = Utf8PathBuf::from("target.txt");
        let preserved_path = Utf8PathBuf::from(".target.txt.moyai-backup");
        let primary_error = crate::error::ToolError::Edit(crate::error::EditError::PartialCommit {
            path: path.clone(),
            preserved_path: preserved_path.clone(),
            reason: "safe retirement was unavailable".to_string(),
        });
        let rollback_error =
            crate::error::ToolError::Edit(crate::error::EditError::RollbackFailed {
                operation: "apply_patch atomic commit".to_string(),
                details: "rollback failure ".repeat(1_024),
            });

        let error = super::merge_patch_mutation_and_rollback_errors(primary_error, rollback_error);
        let crate::error::ToolError::Edit(crate::error::EditError::PartialCommit {
            path: actual_path,
            preserved_path: actual_preserved_path,
            reason,
        }) = error
        else {
            panic!("primary partial commit must remain the typed error");
        };

        assert_eq!(actual_path, path);
        assert_eq!(actual_preserved_path, preserved_path);
        assert!(reason.contains("safe retirement was unavailable"));
        assert!(reason.contains("rollback of earlier patch mutations also failed"));
        assert!(reason.len() <= super::MAX_PARTIAL_COMMIT_REASON_BYTES);
    }

    #[test]
    fn preserved_commit_conflict_merge_keeps_primary_recovery_identity() {
        let path = Utf8PathBuf::from("target.txt");
        let preserved_path = Utf8PathBuf::from(".target.txt.moyai-backup");
        let primary_error =
            crate::error::ToolError::Edit(crate::error::EditError::CommitConflictPreserved {
                path: path.clone(),
                preserved_path: preserved_path.clone(),
                reason: "target was occupied before restore".to_string(),
            });
        let rollback_error =
            crate::error::ToolError::Edit(crate::error::EditError::RollbackFailed {
                operation: "apply_patch atomic commit".to_string(),
                details: "earlier add rollback preserved another path ".repeat(1_024),
            });

        let error = super::merge_patch_mutation_and_rollback_errors(primary_error, rollback_error);
        let crate::error::ToolError::Edit(crate::error::EditError::CommitConflictPreserved {
            path: actual_path,
            preserved_path: actual_preserved_path,
            reason,
        }) = error
        else {
            panic!("primary preserved commit conflict must remain the typed error");
        };

        assert_eq!(actual_path, path);
        assert_eq!(actual_preserved_path, preserved_path);
        assert!(reason.contains("target was occupied before restore"));
        assert!(reason.contains("rollback of earlier patch mutations also failed"));
        assert!(reason.len() <= super::MAX_PARTIAL_COMMIT_REASON_BYTES);
    }

    #[test]
    fn mixed_patch_final_content_preflight_rejects_oversized_later_write() {
        let first = Utf8PathBuf::from("first.txt");
        let deleted = Utf8PathBuf::from("deleted.txt");
        let oversized = Utf8PathBuf::from("oversized.txt");
        let identity = crate::edit::FileContentIdentity {
            mtime_ms: None,
            size_bytes: 3,
            content_sha256: "identity".to_string(),
        };
        let planned = vec![
            super::AdmittedPatchOperation::Add {
                path: first,
                formatted: "1234".to_string(),
            },
            super::AdmittedPatchOperation::Delete {
                path: deleted,
                original: "old".to_string(),
                identity: identity.clone(),
            },
            super::AdmittedPatchOperation::Update {
                source_path: Utf8PathBuf::from("source.txt"),
                destination: oversized.clone(),
                original: "old".to_string(),
                formatted: "12345".to_string(),
                source_identity: identity,
            },
        ];

        let error = super::validate_admitted_patch_final_content_limits(&planned, 4)
            .expect_err("all final writes must pass the turn-fixed limit before commit staging");

        assert!(error.to_string().contains(oversized.as_str()));
        assert!(
            error
                .to_string()
                .contains("configured edit read limit of 4 bytes")
        );
    }

    #[test]
    fn patch_participant_bound_is_enforced_before_mutation() {
        let mut participants = Vec::new();
        for index in 0..super::MAX_APPLY_PATCH_PARTICIPANTS {
            super::claim_apply_patch_participant(
                &mut participants,
                camino::Utf8Path::new(&format!("participant-{index}.txt")),
                "test",
            )
            .expect("participant within recovery bound");
        }

        let error = super::claim_apply_patch_participant(
            &mut participants,
            camino::Utf8Path::new("participant-over-limit.txt"),
            "test",
        )
        .expect_err("participant beyond the recovery bound must fail admission");

        assert_eq!(
            super::MAX_APPLY_PATCH_PARTICIPANTS * 2,
            crate::tool::write_support::MAX_EDIT_RECOVERY_PATHS
        );
        assert!(
            error
                .to_string()
                .contains("split the patch before any mutation")
        );
    }

    #[test]
    fn ordinary_primary_failure_keeps_rollback_typed_recovery_path_and_reason() {
        let path = Utf8PathBuf::from("target.txt");
        let preserved_path = Utf8PathBuf::from(".target.txt.rollback-backup");
        let primary_error = crate::error::ToolError::Message("primary mutation failed".to_string());
        let rollback_error =
            crate::error::ToolError::Edit(crate::error::EditError::RollbackConflictPreserved {
                path: path.clone(),
                preserved_path: preserved_path.clone(),
                reason: "rollback safe retirement failed".to_string(),
            });

        let error = super::merge_patch_mutation_and_rollback_errors(primary_error, rollback_error);
        let crate::error::ToolError::Edit(crate::error::EditError::RollbackConflictPreserved {
            path: actual_path,
            preserved_path: actual_preserved_path,
            reason,
        }) = error
        else {
            panic!("rollback typed recovery identity must win over an ordinary primary error");
        };

        assert_eq!(actual_path, path);
        assert_eq!(actual_preserved_path, preserved_path);
        assert!(reason.contains("primary mutation failed"));
        assert!(reason.contains("rollback safe retirement failed"));
        assert!(reason.len() <= super::MAX_PARTIAL_COMMIT_REASON_BYTES);
    }

    #[test]
    fn distinct_primary_and_rollback_recovery_paths_are_stably_deduplicated_and_typed() {
        let primary_target = Utf8PathBuf::from("primary.txt");
        let primary_backup = Utf8PathBuf::from(".primary.backup");
        let rollback_backup = Utf8PathBuf::from(".rollback.backup");
        let primary_error = crate::error::ToolError::Edit(crate::error::EditError::PartialCommit {
            path: primary_target.clone(),
            preserved_path: primary_backup.clone(),
            reason: "primary reason ".repeat(1_024),
        });
        let rollback_error =
            crate::error::ToolError::Edit(crate::error::EditError::RollbackConflictPreserved {
                path: Utf8PathBuf::from("earlier.txt"),
                preserved_path: rollback_backup.clone(),
                reason: "rollback reason ".repeat(1_024),
            });

        let error = super::merge_patch_mutation_and_rollback_errors(primary_error, rollback_error);
        let crate::error::ToolError::Edit(crate::error::EditError::RecoveryFilesPreserved {
            path,
            preserved_paths,
            reason,
        }) = error
        else {
            panic!("distinct recovery paths must use the bounded multi-path error");
        };

        assert_eq!(path, primary_target);
        assert_eq!(preserved_paths, vec![primary_backup, rollback_backup]);
        assert!(reason.contains(super::PATCH_ROLLBACK_FAILURE_LABEL));
        assert!(reason.len() <= super::MAX_PARTIAL_COMMIT_REASON_BYTES);
    }

    #[test]
    fn duplicate_recovery_path_keeps_single_existing_variant() {
        let target = Utf8PathBuf::from("target.txt");
        let shared_backup = Utf8PathBuf::from(".shared.backup");
        let primary_error = crate::error::ToolError::Edit(crate::error::EditError::PartialCommit {
            path: target.clone(),
            preserved_path: shared_backup.clone(),
            reason: "primary failure".to_string(),
        });
        let rollback_error =
            crate::error::ToolError::Edit(crate::error::EditError::RollbackConflictPreserved {
                path: Utf8PathBuf::from("earlier.txt"),
                preserved_path: shared_backup.clone(),
                reason: "rollback failure".to_string(),
            });

        let error = super::merge_patch_mutation_and_rollback_errors(primary_error, rollback_error);
        let crate::error::ToolError::Edit(crate::error::EditError::PartialCommit {
            path,
            preserved_path,
            reason,
        }) = error
        else {
            panic!("a deduplicated single path must keep the primary existing variant");
        };

        assert_eq!(path, target);
        assert_eq!(preserved_path, shared_backup);
        assert!(reason.contains("primary failure"));
        assert!(reason.contains("rollback failure"));
        assert!(reason.len() <= super::MAX_PARTIAL_COMMIT_REASON_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn add_then_update_or_delete_keeps_both_primary_and_rollback_recovery_paths() {
        for delete_existing in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root =
                Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 workspace");
            let added = root.join("added.txt");
            let existing = root.join("existing.txt");
            let workspace = test_workspace(&root);
            std::fs::write(&existing, "before").expect("seed existing file");
            let (_, existing_identity) =
                read_file_with_identity(&existing, 1_024).expect("existing identity");
            let terminal_mutation = if delete_existing {
                PatchMutation::Delete {
                    path: existing.clone(),
                    expected_identity: existing_identity,
                    rollback: FileRollbackState::Present("before".to_string()),
                }
            } else {
                PatchMutation::Write {
                    path: existing.clone(),
                    text: "after".to_string(),
                    expected_identity: Some(existing_identity),
                    rollback: FileRollbackState::Present("before".to_string()),
                }
            };
            let mutations = vec![
                PatchMutation::Write {
                    path: added.clone(),
                    text: "added".to_string(),
                    expected_identity: None,
                    rollback: FileRollbackState::Absent,
                },
                terminal_mutation,
            ];

            let (primary_error, applied) =
                apply_patch_mutations(&EditSafety::default(), &mutations, &workspace)
                    .expect_err("Unix retirement must surface a partial commit");
            assert_eq!(applied.len(), 1);
            let error = super::rollback_failed_patch_mutations(
                primary_error,
                &mutations,
                &applied,
                &workspace,
            );
            let crate::error::ToolError::Edit(crate::error::EditError::RecoveryFilesPreserved {
                path,
                preserved_paths,
                reason,
            }) = error
            else {
                panic!("primary and rollback recovery paths must both remain typed");
            };

            assert_eq!(path, existing);
            assert_eq!(preserved_paths.len(), 2);
            assert_eq!(
                std::fs::read_to_string(&preserved_paths[0]).expect("read primary recovery path"),
                "before"
            );
            assert_eq!(
                std::fs::read_to_string(&preserved_paths[1]).expect("read rollback recovery path"),
                "added"
            );
            assert!(reason.contains("rollback of earlier patch mutations also failed"));
            assert!(reason.len() <= super::MAX_PARTIAL_COMMIT_REASON_BYTES);
            assert!(!added.exists());
            if delete_existing {
                assert!(!existing.exists());
            } else {
                assert_eq!(
                    std::fs::read_to_string(&existing).expect("read partial update"),
                    "after"
                );
            }
        }
    }
}
