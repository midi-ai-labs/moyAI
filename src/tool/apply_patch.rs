use std::fs;

use async_trait::async_trait;
use camino::Utf8Path;
use serde::Deserialize;
use serde_json::json;

use crate::edit::{
    FileContentIdentity, FormatterExecutionOptions, PatchOperation, PatchParser,
    path_for_change_storage,
};
use crate::error::ToolError;
use crate::session::ChangeRepository;
use crate::tool::context::{ToolContext, ToolEffectAdmission};
use crate::tool::registry::Tool;
use crate::tool::write_support::{
    delete_file_conditionally, read_text_file_with_identity, to_summary,
    write_text_file_conditionally,
};
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, GuardedPath, PathGuard, is_protected_workspace_authority_path};

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
        let permission_admission = build_patch_permission_admission(&ctx, operations.as_slice())?;
        let effect_admission =
            confirm_patch_permission_admission(&mut ctx, &permission_admission).await?;
        let lock_paths = apply_patch_tool_invocation_lock_paths(&ctx, operations.as_slice())?;
        let edit_safety = ctx.services.edit_safety.clone();
        edit_safety
            .with_file_locks(&lock_paths, async move {
                execute_admitted_patch_operations(&mut ctx, operations, effect_admission).await
            })
            .await
    }
}

async fn execute_admitted_patch_operations(
    ctx: &mut ToolContext<'_>,
    operations: Vec<PatchOperation>,
    effect_admission: ToolEffectAdmission,
) -> Result<ToolResult, ToolError> {
    ctx.run_mutation_fence.assert_owned().await?;
    let admission = classify_patch_operations_before_side_effects(
        ctx,
        operations.as_slice(),
        &effect_admission,
    )
    .await?;
    if let Some(path) = admission.all_update_operations_no_content_path {
        return Ok(no_content_patch_result(&path, &ctx.workspace.root));
    }
    let first_no_content_update_path = admission.first_no_content_update_path.clone();
    let planned_operations = admission.planned_operations;
    let commit = stage_admitted_patch_commit(ctx, planned_operations)?;

    if commit.changes.is_empty() {
        let path = first_no_content_update_path.unwrap_or_else(|| ctx.workspace.root.clone());
        return Ok(no_content_patch_result(&path, &ctx.workspace.root));
    }

    let change_ids = commit_admitted_patch(ctx, &commit).await?;

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
                ctx.services.change_tracker.build_change(
                    tool_call_id,
                    None,
                    Some(stored_path.as_ref()),
                    None,
                    Some(&formatted),
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
                ctx.services.change_tracker.build_change(
                    tool_call_id,
                    Some(stored_source_path.as_ref()),
                    Some(stored_destination_path.as_ref()),
                    Some(&original),
                    Some(&formatted),
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
                ctx.services.change_tracker.build_change(
                    tool_call_id,
                    Some(stored_path.as_ref()),
                    None,
                    Some(&original),
                    None,
                )
            }
        }
        .map_err(ToolError::from)?;
        summaries.push(to_summary(&change));
        changes.push(change);
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
                rollback_patch_commit(&commit.mutations, &applied, ctx.workspace, None)?;
                return Err(error);
            }
        };

    if let Err(error) = ctx.services.edit_safety.sync_file_mutations(
        session_id,
        &commit.removed_paths,
        &commit.current_paths,
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
    let mut rollback_errors = Vec::new();
    for applied_mutation in applied.iter().rev() {
        let mutation = &mutations[applied_mutation.mutation_index];
        if let Err(error) =
            rollback_patch_mutation(mutation, &applied_mutation.committed_state, workspace)
        {
            rollback_errors.push(error.to_string());
        }
    }
    if rollback_errors.is_empty()
        && let Some((edit_safety, session_id, snapshot)) = baseline_snapshot
    {
        if let Err(error) = edit_safety.restore_path_stamps(session_id, snapshot) {
            rollback_errors.push(error.to_string());
        }
    }
    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(ToolError::from(crate::error::EditError::RollbackFailed {
            operation: "apply_patch atomic commit".to_string(),
            details: rollback_errors.join("; "),
        }))
    }
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
        crate::error::EditError::CommitConflictPreserved { preserved_path, .. } => {
            ToolError::from(crate::error::EditError::RollbackConflictPreserved {
                path: path.to_path_buf(),
                preserved_path,
            })
        }
        crate::error::EditError::CommitConflict { .. } => {
            ToolError::from(crate::error::EditError::RollbackConflict {
                path: path.to_path_buf(),
            })
        }
        crate::error::EditError::PartialCommit { preserved_path, .. } => {
            ToolError::from(crate::error::EditError::RollbackConflictPreserved {
                path: path.to_path_buf(),
                preserved_path,
            })
        }
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

#[derive(Debug, Default)]
struct PatchPermissionAdmission {
    targets: Vec<camino::Utf8PathBuf>,
    outside_workspace: bool,
    risks: Vec<PermissionRisk>,
    details: Vec<String>,
}

async fn classify_patch_operations_before_side_effects(
    ctx: &ToolContext<'_>,
    operations: &[PatchOperation],
    effect_admission: &ToolEffectAdmission,
) -> Result<PatchOperationAdmission, ToolError> {
    let mut saw_update = false;
    let mut saw_non_update = false;
    let mut saw_content_changing_update = false;
    let mut no_content_path = None;
    let mut planned_operations = Vec::new();
    for operation in operations {
        match operation {
            PatchOperation::Add { path, contents } => {
                debug_assert!(patch_operation_requires_pre_side_effect_admission(
                    operation
                ));
                saw_non_update = true;
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                if guarded.absolute.exists() {
                    return Err(ToolError::from(crate::error::EditError::Message(
                        add_existing_path_message(&guarded.absolute),
                    )));
                }
                let normalized = ctx.services.formatter.normalize_text(
                    &guarded.absolute,
                    None,
                    contents.clone(),
                )?;
                let formatted = effect_admission
                    .format_if_configured(
                        &ctx.services.formatter,
                        &guarded.absolute,
                        normalized.clone(),
                        formatter_execution_options(ctx, normalized.len()),
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
                validate_move_destination_admission(&source_path, &destination)?;
                let (original, source_identity) = read_text_file_with_identity(&source_path)?;
                ctx.services.edit_safety.assert_fresh_write(
                    ctx.session.session.id,
                    &source_path,
                    &source_identity,
                )?;
                let patched = PatchParser::apply_to_text(&original, hunks)
                    .map_err(|error| crate::error::EditError::Message(error.to_string()))?;
                let normalized = ctx.services.formatter.normalize_text(
                    &destination,
                    Some(&original),
                    patched,
                )?;
                let formatted = effect_admission
                    .format_if_configured(
                        &ctx.services.formatter,
                        &destination,
                        normalized.clone(),
                        formatter_execution_options(ctx, normalized.len()),
                    )
                    .await?;
                if update_operation_is_no_content(&formatted, &original, &destination, &source_path)
                {
                    no_content_path.get_or_insert(destination);
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
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                let (original, identity) = read_text_file_with_identity(&guarded.absolute)?;
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
    Ok(PatchOperationAdmission {
        first_no_content_update_path: no_content_path,
        all_update_operations_no_content_path,
        planned_operations,
    })
}

fn patch_operation_requires_pre_side_effect_admission(operation: &PatchOperation) -> bool {
    matches!(
        operation,
        PatchOperation::Add { .. } | PatchOperation::Update { .. } | PatchOperation::Delete { .. }
    )
}

fn formatter_execution_options(
    ctx: &ToolContext<'_>,
    input_bytes: usize,
) -> FormatterExecutionOptions {
    let output_slack =
        usize::try_from(ctx.config.file_guard.max_inline_read_bytes).unwrap_or(usize::MAX);
    FormatterExecutionOptions {
        workspace_root: ctx.workspace.root.clone(),
        timeout_ms: ctx
            .config
            .shell
            .default_timeout_ms
            .min(ctx.config.shell.max_timeout_ms),
        max_output_bytes: input_bytes.saturating_add(output_slack),
        cancel: ctx.cancel.clone(),
    }
}

fn build_patch_permission_admission(
    ctx: &ToolContext<'_>,
    operations: &[PatchOperation],
) -> Result<PatchPermissionAdmission, ToolError> {
    let mut admission = PatchPermissionAdmission::default();
    for operation in operations {
        match operation {
            PatchOperation::Add { path, .. } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                extend_patch_permission_admission(
                    &mut admission,
                    ctx.workspace.root.as_path(),
                    &guarded,
                    None,
                    false,
                    false,
                    "add",
                );
            }
            PatchOperation::Update { path, move_to, .. } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                let move_guard = move_to
                    .as_ref()
                    .map(|target| PathGuard::require_path(ctx.workspace, target, AccessKind::Edit))
                    .transpose()?;
                let move_or_rename = move_guard
                    .as_ref()
                    .is_some_and(|target| target.absolute != guarded.absolute);
                extend_patch_permission_admission(
                    &mut admission,
                    ctx.workspace.root.as_path(),
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
            }
            PatchOperation::Delete { path } => {
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                extend_patch_permission_admission(
                    &mut admission,
                    ctx.workspace.root.as_path(),
                    &guarded,
                    None,
                    true,
                    false,
                    "delete",
                );
            }
        }
    }
    Ok(admission)
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
        AccessKind::Edit,
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
    if is_protected_workspace_authority_path(workspace_root, &guarded.absolute)
        || move_guard.as_ref().is_some_and(|target| {
            is_protected_workspace_authority_path(workspace_root, &target.absolute)
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

    use crate::config::{FormatConfig, FormatterRule, NewlineStyle};
    use crate::edit::{EditSafety, Formatter, FormatterExecutionOptions, read_file_with_identity};
    use crate::protocol::TurnInterruptionCause;
    use crate::runtime::{RunCancelOutcome, RunCancellationCause, RunControl};
    use crate::tool::context::ToolEffectAdmission;
    use crate::workspace::{Workspace, WorkspaceDiscovery};

    use super::{
        CommittedFileState, FileRollbackState, PatchMutation, apply_patch_mutations,
        restore_file_state,
    };

    fn test_workspace(root: &camino::Utf8Path) -> Workspace {
        WorkspaceDiscovery::discover_fixed_root(root, &crate::config::ResolvedConfig::default())
            .expect("test workspace")
    }

    fn marker_formatter() -> Formatter {
        Formatter::new(FormatConfig {
            default_newline: NewlineStyle::Lf,
            ensure_trailing_newline: true,
            commands: vec![FormatterRule {
                glob: "**/*.txt".to_string(),
                command: marker_command(),
            }],
        })
    }

    fn formatter_options(root: Utf8PathBuf, control: &RunControl) -> FormatterExecutionOptions {
        FormatterExecutionOptions {
            workspace_root: root,
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
            let formatter = marker_formatter();

            admission
                .format_if_configured(
                    &formatter,
                    &first_path,
                    "first".to_string(),
                    formatter_options(root.clone(), &control),
                )
                .await
                .expect("first formatter effect");
            assert!(first_dir.join("formatter-started.marker").exists());
            assert_eq!(control.request_cancel(cause), RunCancelOutcome::Applied);

            let error = admission
                .format_if_configured(
                    &formatter,
                    &second_path,
                    "second".to_string(),
                    formatter_options(root.clone(), &control),
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
        let (_, expected_identity) = read_file_with_identity(&path).expect("capture identity");
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
        let (_, first_identity) = read_file_with_identity(&first).expect("first identity");
        let (_, second_identity) = read_file_with_identity(&second).expect("second identity");
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
    fn rollback_cas_preserves_external_rewrite_after_patch_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        let workspace = test_workspace(path.parent().expect("parent"));
        std::fs::write(&path, "agent").expect("agent write");
        let (_, identity) = read_file_with_identity(&path).expect("agent identity");
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
}
