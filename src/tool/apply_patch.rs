use std::fs;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::Utf8Path;
use serde::Deserialize;
use serde_json::json;

use crate::edit::{PatchChunk, PatchOperation, PatchParser, path_for_change_storage};
use crate::error::ToolError;
use crate::session::ChangeRepository;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::write_support::{to_summary, write_text_file};
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
        confirm_patch_permission_admission(&mut ctx, &permission_admission)?;
        let lock_paths = apply_patch_tool_invocation_lock_paths(&ctx, operations.as_slice())?;
        let edit_safety = ctx.services.edit_safety.clone();
        edit_safety
            .with_file_locks(&lock_paths, async move {
                execute_admitted_patch_operations(&mut ctx, operations).await
            })
            .await
    }
}

async fn execute_admitted_patch_operations(
    ctx: &mut ToolContext<'_>,
    operations: Vec<PatchOperation>,
) -> Result<ToolResult, ToolError> {
    let admission =
        classify_patch_operations_before_side_effects(ctx, operations.as_slice()).await?;
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
            .map(|summary| summary.tool_feedback_text(Some(&ctx.workspace.root)))
            .collect::<Vec<_>>()
            .join("\n"),
        metadata: json!({
            "changes": commit.summaries.iter().map(|summary| json!({
                "change_id": summary.change_id,
                "kind": summary.kind,
                "path_before": summary.path_before,
                "path_after": summary.path_after
            })).collect::<Vec<_>>(),
            "diff_text": commit.changes.iter().map(|change| change.diff_text.clone()).collect::<Vec<_>>().join("\n")
        }),
        truncated_output_path: None,
        recorded_changes: change_ids,
        change_summaries: commit.summaries.clone(),
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
            } => {
                let stored_source_path =
                    path_for_apply_patch_change_storage(&source_path, &workspace_root);
                let stored_destination_path =
                    path_for_apply_patch_change_storage(&destination, &workspace_root);
                mutations.push(PatchMutation::Write {
                    path: destination.clone(),
                    text: formatted.clone(),
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
            AdmittedPatchOperation::Delete { path, original } => {
                let stored_path = path_for_apply_patch_change_storage(&path, &workspace_root);
                mutations.push(PatchMutation::Delete {
                    path: path.clone(),
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

    let applied_count = match apply_patch_mutations(&commit.mutations) {
        Ok(value) => value,
        Err((error, applied_count)) => {
            rollback_patch_commit(&commit.mutations, applied_count, None)?;
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
            applied_count,
            Some((&ctx.services.edit_safety, session_id, &baseline_snapshot)),
        )?;
        return Err(ToolError::from(error));
    }

    match ctx
        .services
        .store
        .change_repo()
        .insert_changes(session_id, &commit.changes)
        .await
    {
        Ok(change_ids) => Ok(change_ids),
        Err(error) => {
            rollback_patch_commit(
                &commit.mutations,
                applied_count,
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
    },
    Delete {
        path: camino::Utf8PathBuf,
        original: String,
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
        rollback: FileRollbackState,
    },
    Delete {
        path: camino::Utf8PathBuf,
        rollback: FileRollbackState,
    },
}

#[derive(Debug)]
enum FileRollbackState {
    Absent,
    Present(String),
}

fn apply_patch_mutations(mutations: &[PatchMutation]) -> Result<usize, (ToolError, usize)> {
    let mut applied_count = 0;
    for mutation in mutations {
        if let Err(error) = apply_patch_mutation(mutation) {
            return Err((error, applied_count));
        }
        applied_count += 1;
    }
    Ok(applied_count)
}

fn apply_patch_mutation(mutation: &PatchMutation) -> Result<(), ToolError> {
    match mutation {
        PatchMutation::Write { path, text, .. } => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_text_file(path, text)?;
        }
        PatchMutation::Delete { path, .. } => {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn rollback_patch_commit(
    mutations: &[PatchMutation],
    applied_count: usize,
    baseline_snapshot: Option<(
        &crate::edit::EditSafety,
        crate::session::SessionId,
        &[(camino::Utf8PathBuf, Option<crate::edit::FileReadStamp>)],
    )>,
) -> Result<(), ToolError> {
    let mut rollback_errors = Vec::new();
    for mutation in mutations.iter().take(applied_count).rev() {
        if let Err(error) = rollback_patch_mutation(mutation) {
            rollback_errors.push(error.to_string());
        }
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
            "apply_patch atomic commit rollback failed: {}",
            rollback_errors.join("; ")
        ))))
    }
}

fn rollback_patch_mutation(mutation: &PatchMutation) -> Result<(), ToolError> {
    match mutation {
        PatchMutation::Write { path, rollback, .. } | PatchMutation::Delete { path, rollback } => {
            restore_file_state(path, rollback)
        }
    }
}

fn restore_file_state(path: &Utf8Path, state: &FileRollbackState) -> Result<(), ToolError> {
    match state {
        FileRollbackState::Absent => {
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        FileRollbackState::Present(text) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_text_file(path, text)?;
        }
    }
    Ok(())
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
                let formatted = ctx
                    .services
                    .formatter
                    .format_if_configured(&guarded.absolute, normalized)
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
                let original = fs::read_to_string(&source_path)?;
                let metadata = fs::metadata(&source_path)?;
                let current_mtime_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| value.as_millis() as i64);
                ctx.services.edit_safety.assert_fresh_write(
                    ctx.session.session.id,
                    &source_path,
                    current_mtime_ms,
                    Some(metadata.len()),
                )?;
                let patched = PatchParser::apply_to_text(&original, hunks)
                    .map_err(|error| crate::error::EditError::Message(error.to_string()))?;
                let normalized = ctx.services.formatter.normalize_text(
                    &destination,
                    Some(&original),
                    patched,
                )?;
                let formatted = ctx
                    .services
                    .formatter
                    .format_if_configured(&destination, normalized)
                    .await?;
                if update_operation_is_no_content(&formatted, &original, &destination, &source_path)
                {
                    no_content_path.get_or_insert(destination);
                    continue;
                }
                if let Some(message) =
                    suspicious_full_rewrite_message(&original, &formatted, hunks, &destination)
                {
                    return Err(ToolError::from(crate::error::EditError::Message(message)));
                }
                saw_content_changing_update = true;
                planned_operations.push(AdmittedPatchOperation::Update {
                    source_path,
                    destination,
                    original,
                    formatted,
                });
            }
            PatchOperation::Delete { path } => {
                debug_assert!(patch_operation_requires_pre_side_effect_admission(
                    operation
                ));
                saw_non_update = true;
                let guarded = PathGuard::require_path(ctx.workspace, path, AccessKind::Edit)?;
                let original = fs::read_to_string(&guarded.absolute)?;
                let metadata = fs::metadata(&guarded.absolute)?;
                let current_mtime_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| value.as_millis() as i64);
                ctx.services.edit_safety.assert_fresh_write(
                    ctx.session.session.id,
                    &guarded.absolute,
                    current_mtime_ms,
                    Some(metadata.len()),
                )?;
                planned_operations.push(AdmittedPatchOperation::Delete {
                    path: guarded.absolute,
                    original,
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

fn confirm_patch_permission_admission(
    ctx: &mut ToolContext<'_>,
    admission: &PatchPermissionAdmission,
) -> Result<(), ToolError> {
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
            "path `{path}` is targeted by multiple content-changing operations in the same apply_patch ToolInvocation. Split repeated edits to the same file into separate tool calls so each invocation has one owner per mutation participant; duplicate owner: {operation_name}."
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
            "move destination `{destination}` already exists. `*** Move to:` cannot implicitly overwrite an existing file; read and update or delete the destination explicitly before moving."
        )));
    }
    Ok(())
}

fn add_existing_path_message(path: &Utf8Path) -> String {
    format!(
        "path `{path}` already exists; use `*** Update File: {path}` to modify an existing file instead of `*** Add File`"
    )
}

fn suspicious_full_rewrite_message(
    original: &str,
    updated: &str,
    hunks: &[PatchChunk],
    path: &Utf8Path,
) -> Option<String> {
    if !PatchParser::is_full_rewrite(hunks) {
        return None;
    }
    if substantive_artifact_collapsed_to_noop_acknowledgement(original, updated) {
        return Some(format!(
            "full-file rewrite for `{path}` would replace a substantive artifact with a no-op acknowledgement. Do not patch files to say they are already up to date; leave the file unchanged or make a real content update."
        ));
    }
    let original_first = first_nonempty_line(original)?;
    let updated_first = first_nonempty_line(updated)?;
    if starts_with_indentation(updated_first) && !starts_with_indentation(original_first) {
        return Some(format!(
            "full-file rewrite for `{path}` appears to start in the middle of the file at `{}`. Resend the entire file from its real first line in one `*** Update File: {path}` patch using only inserted lines (`+...`).",
            updated_first.trim()
        ));
    }
    None
}

fn no_content_patch_message(path: &Utf8Path) -> String {
    format!(
        "apply_patch made no content changes to `{path}`. No file-change evidence was produced; submit a patch with actual content changes or leave the file unchanged."
    )
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
            "success": false,
            "progress_effect": "no_progress",
            "tool_feedback_envelope": {
                "success": false,
                "progress_effect": "no_progress",
                "tool": "apply_patch",
                "target": display_path,
                "side_effects_applied": false
            },
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

fn path_for_apply_patch_change_storage(
    path: &Utf8Path,
    workspace_root: &Utf8Path,
) -> camino::Utf8PathBuf {
    path_for_change_storage(path, workspace_root)
}

fn substantive_artifact_collapsed_to_noop_acknowledgement(original: &str, updated: &str) -> bool {
    let original_lines = meaningful_line_count(original);
    let updated_lines = meaningful_line_count(updated);
    original_lines >= 8 && updated_lines <= 3 && is_noop_acknowledgement_text(updated)
}

fn meaningful_line_count(text: &str) -> usize {
    text.lines().filter(|line| !line.trim().is_empty()).count()
}

fn is_noop_acknowledgement_text(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| {
            ch == '-'
                || ch == '*'
                || ch == '#'
                || ch == '`'
                || ch == '"'
                || ch == '\''
                || ch.is_whitespace()
        })
        .to_lowercase();
    let normalized = normalized.replace(['_', '-'], " ");
    [
        "content is already up to date",
        "already up to date",
        "no changes needed",
        "no changes required",
        "unchanged",
        "変更なし",
        "更新済み",
        "変更はありません",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines().find(|line| !line.trim().is_empty())
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

fn starts_with_indentation(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

#[cfg(test)]
mod tests {}
