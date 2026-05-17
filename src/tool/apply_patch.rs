use std::fs;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::Utf8Path;
use serde::Deserialize;
use serde_json::json;

use crate::edit::{PatchChunk, PatchLine, PatchOperation, PatchParser};
use crate::error::ToolError;
use crate::session::ChangeRepository;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::write_support::{build_read_stamp, to_summary, write_text_file};
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{
    AccessKind, GuardedPath, PathGuard, is_protected_instruction_or_config_path,
};

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
                        "description": "Entire patch text. Must start with `*** Begin Patch` and end with `*** End Patch`. For new files, use `*** Add File: path` and prefix every added line with `+`. Do not use unified diff markers like `---` or `+++`."
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
        let operations = PatchParser::parse(&input.patch_text).map_err(guided_patch_error)?;
        let mut changes = Vec::new();
        let mut summaries = Vec::new();
        let mut paths_to_invalidate = Vec::new();

        for operation in operations {
            match &operation {
                PatchOperation::Add { path, .. } => {
                    let guarded =
                        PathGuard::require_path(ctx.workspace, path, AccessKind::Edit, true)?;
                    ctx.confirm_if_needed(
                        AccessKind::Edit,
                        format!("Apply patch to {}", guarded.absolute),
                        vec![guarded.absolute.clone()],
                        edit_request_is_outside_workspace(&guarded, None),
                        edit_request_risks(
                            ctx.workspace.root.as_path(),
                            &guarded,
                            None,
                            false,
                            false,
                        ),
                    )?;
                    let change = apply_add(&mut ctx, &guarded.absolute, &operation).await?;
                    summaries.push(to_summary(&change));
                    changes.push(change);
                }
                PatchOperation::Update { path, move_to, .. } => {
                    let guarded =
                        PathGuard::require_path(ctx.workspace, path, AccessKind::Edit, true)?;
                    let move_guard = move_to
                        .as_ref()
                        .map(|target| {
                            PathGuard::require_path(ctx.workspace, target, AccessKind::Edit, true)
                        })
                        .transpose()?;
                    let mut targets = vec![guarded.absolute.clone()];
                    if let Some(target) = &move_guard {
                        targets.push(target.absolute.clone());
                    }
                    ctx.confirm_if_needed(
                        AccessKind::Edit,
                        format!("Apply patch to {}", guarded.absolute),
                        targets,
                        edit_request_is_outside_workspace(&guarded, move_guard.as_ref()),
                        edit_request_risks(
                            ctx.workspace.root.as_path(),
                            &guarded,
                            move_guard.as_ref(),
                            false,
                            move_guard
                                .as_ref()
                                .is_some_and(|target| target.absolute != guarded.absolute),
                        ),
                    )?;
                    let change = apply_update(
                        &mut ctx,
                        &guarded.absolute,
                        move_guard.as_ref().map(|value| value.absolute.as_ref()),
                        &operation,
                    )
                    .await?;
                    if let Some(target) = move_guard {
                        if target.absolute != guarded.absolute {
                            paths_to_invalidate.push(guarded.absolute.clone());
                        }
                    }
                    summaries.push(to_summary(&change));
                    changes.push(change);
                }
                PatchOperation::Delete { path } => {
                    let guarded =
                        PathGuard::require_path(ctx.workspace, path, AccessKind::Edit, true)?;
                    ctx.confirm_if_needed(
                        AccessKind::Edit,
                        format!("Delete {}", guarded.absolute),
                        vec![guarded.absolute.clone()],
                        edit_request_is_outside_workspace(&guarded, None),
                        edit_request_risks(
                            ctx.workspace.root.as_path(),
                            &guarded,
                            None,
                            true,
                            false,
                        ),
                    )?;
                    let change = apply_delete(&mut ctx, &guarded.absolute).await?;
                    paths_to_invalidate.push(guarded.absolute.clone());
                    summaries.push(to_summary(&change));
                    changes.push(change);
                }
            }
        }

        let change_ids = ctx
            .services
            .store
            .change_repo()
            .insert_changes(ctx.session.session.id, &changes)
            .await?;
        if !paths_to_invalidate.is_empty() {
            ctx.services
                .edit_safety
                .invalidate_paths(ctx.session.session.id, &paths_to_invalidate)?;
        }

        Ok(ToolResult {
            title: format!("Applied {} change(s)", changes.len()),
            output_text: summaries
                .iter()
                .map(|summary| summary.tool_feedback_text(Some(&ctx.workspace.root)))
                .collect::<Vec<_>>()
                .join("\n"),
            metadata: json!({
                "changes": summaries.iter().map(|summary| json!({
                    "change_id": summary.change_id,
                    "kind": summary.kind,
                    "path_before": summary.path_before,
                    "path_after": summary.path_after
                })).collect::<Vec<_>>(),
                "diff_text": changes.iter().map(|change| change.diff_text.clone()).collect::<Vec<_>>().join("\n")
            }),
            truncated_output_path: None,
            recorded_changes: change_ids,
            change_summaries: summaries,
        })
    }
}

fn guided_patch_error(error: crate::error::PatchError) -> ToolError {
    ToolError::Patch(crate::error::PatchError::Message(format!(
        "{error}. Use the exact apply_patch grammar, for example:\n*** Begin Patch\n*** Add File: notes.txt\n+hello\n*** End Patch\nIf the target file already exists, switch to `*** Update File: path` instead of `*** Add File`."
    )))
}

async fn apply_add(
    ctx: &mut ToolContext<'_>,
    path: &Utf8Path,
    operation: &PatchOperation,
) -> Result<crate::edit::FileChange, ToolError> {
    let PatchOperation::Add { contents, .. } = operation else {
        return Err(ToolError::Message("expected add operation".to_string()));
    };
    let services = ctx.services.clone();
    let session_id = ctx.session.session.id;
    let tool_call_id = ctx.tool_call_id;
    let path_buf = path.to_path_buf();
    let contents = contents.clone();
    let edit_safety = services.edit_safety.clone();
    edit_safety
        .with_file_lock(path, async move {
            if path_buf.exists() {
                return Err(crate::error::EditError::Message(format!(
                    "path `{path_buf}` already exists; use `*** Update File: {path_buf}` to modify an existing file instead of `*** Add File`"
                )));
            }
            if let Some(parent) = path_buf.parent() {
                fs::create_dir_all(parent)?;
            }
            let normalized = services.formatter.normalize_text(path, None, contents)?;
            let formatted = services
                .formatter
                .format_if_configured(path, normalized)
                .await?;
            write_text_file(&path_buf, &formatted)?;
            services
                .edit_safety
                .record_read(session_id, build_read_stamp(&path_buf)?)?;
            services.change_tracker.build_change(
                tool_call_id,
                None,
                Some(&path_buf),
                None,
                Some(&formatted),
            )
        })
        .await
        .map_err(ToolError::from)
}

async fn apply_update(
    ctx: &mut ToolContext<'_>,
    path: &Utf8Path,
    move_to: Option<&Utf8Path>,
    operation: &PatchOperation,
) -> Result<crate::edit::FileChange, ToolError> {
    let PatchOperation::Update { hunks, .. } = operation else {
        return Err(ToolError::Message("expected update operation".to_string()));
    };
    let services = ctx.services.clone();
    let session_id = ctx.session.session.id;
    let tool_call_id = ctx.tool_call_id;
    let source_path = path.to_path_buf();
    let target_path = move_to.map(|value| value.to_path_buf());
    let hunks = hunks.clone();
    let edit_safety = services.edit_safety.clone();
    edit_safety
        .with_file_lock(path, async move {
            let original = fs::read_to_string(&source_path)?;
            let metadata = fs::metadata(&source_path)?;
            let current_mtime_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_millis() as i64);
            services.edit_safety.assert_fresh_write(
                session_id,
                &source_path,
                current_mtime_ms,
                Some(metadata.len()),
            )?;
            let patched = PatchParser::apply_to_text(&original, &hunks)
                .map_err(|error| crate::error::EditError::Message(error.to_string()))?;
            let destination = target_path.clone().unwrap_or_else(|| source_path.clone());
            let normalized =
                services
                    .formatter
                    .normalize_text(&destination, Some(&original), patched)?;
            let formatted = services
                .formatter
                .format_if_configured(&destination, normalized)
                .await?;
            if formatted == original && destination == source_path {
                return Err(crate::error::EditError::Message(no_content_patch_message(
                    &destination,
                )));
            }
            if let Some(message) =
                suspicious_full_rewrite_message(&original, &formatted, &hunks, &destination)
            {
                return Err(crate::error::EditError::Message(message));
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            write_text_file(&destination, &formatted)?;
            if destination != source_path && source_path.exists() {
                fs::remove_file(&source_path)?;
            }
            services
                .edit_safety
                .record_read(session_id, build_read_stamp(&destination)?)?;
            services.change_tracker.build_change(
                tool_call_id,
                Some(&source_path),
                Some(&destination),
                Some(&original),
                Some(&formatted),
            )
        })
        .await
        .map_err(ToolError::from)
}

async fn apply_delete(
    ctx: &mut ToolContext<'_>,
    path: &Utf8Path,
) -> Result<crate::edit::FileChange, ToolError> {
    let services = ctx.services.clone();
    let session_id = ctx.session.session.id;
    let tool_call_id = ctx.tool_call_id;
    let path_buf = path.to_path_buf();
    let edit_safety = services.edit_safety.clone();
    edit_safety
        .with_file_lock(path, async move {
            let original = fs::read_to_string(&path_buf)?;
            let metadata = fs::metadata(&path_buf)?;
            let current_mtime_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_millis() as i64);
            services.edit_safety.assert_fresh_write(
                session_id,
                &path_buf,
                current_mtime_ms,
                Some(metadata.len()),
            )?;
            fs::remove_file(&path_buf)?;
            services
                .edit_safety
                .invalidate_paths(session_id, std::slice::from_ref(&path_buf))?;
            services.change_tracker.build_change(
                tool_call_id,
                Some(&path_buf),
                None,
                Some(&original),
                None,
            )
        })
        .await
        .map_err(ToolError::from)
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
        "apply_patch made no content changes to `{path}`. This is no-progress and cannot satisfy active authoring or verification repair work; leave the file unchanged and continue with a real edit or verification command."
    )
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
    if is_protected_instruction_or_config_path(workspace_root, &guarded.absolute)
        || move_guard.as_ref().is_some_and(|target| {
            is_protected_instruction_or_config_path(workspace_root, &target.absolute)
        })
    {
        risks.push(PermissionRisk::ProtectedInstructionOrConfig);
    }
    risks
}

fn starts_with_indentation(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

pub(crate) fn destructive_noop_patch_is_rejected_fixture_passes() -> bool {
    let original = "# CLI 電卓 設計文書\n\n## 概要\n\n四則演算を行う。\n\n## 関数仕様\n\n- add\n- subtract\n- multiply\n- divide\n- power\n- modulo\n";
    let updated = "--- Content is already up to date ---\n";
    let hunks = vec![PatchChunk {
        old_start: 0,
        old_lines: 0,
        new_start: 0,
        new_lines: 0,
        lines: vec![PatchLine::Insert(
            "--- Content is already up to date ---".to_string(),
        )],
    }];
    let path = Utf8Path::new("docs/calculator-design.md");
    let message =
        suspicious_full_rewrite_message(original, updated, &hunks, path).unwrap_or_default();
    message.contains("no-op acknowledgement")
        && message.contains("leave the file unchanged")
        && !substantive_artifact_collapsed_to_noop_acknowledgement(
            "one line\n",
            "--- Content is already up to date ---\n",
        )
}

pub(crate) fn empty_or_zero_diff_patch_is_rejected_fixture_passes() -> bool {
    let empty_update = "*** Begin Patch\n*** Update File: docs/calculator-design.md\n*** Update File: docs/calculator-design.md\n*** End Patch";
    let empty_rejected = PatchParser::parse(empty_update).err().is_some_and(|error| {
        error
            .to_string()
            .contains("must include at least one hunk line")
    });
    let path = Utf8Path::new("docs/calculator-design.md");
    empty_rejected
        && no_content_patch_message(path).contains("made no content changes")
        && no_content_patch_message(path).contains("cannot satisfy active authoring")
}

pub(crate) fn hunkless_update_patch_is_rejected_fixture_passes() -> bool {
    let hunkless_update =
        "*** Begin Patch\n*** Update File: docs/calculator-design.md\n\n*** End Patch";
    let hunkless_rejected = PatchParser::parse(hunkless_update)
        .err()
        .is_some_and(|error| {
            error
                .to_string()
                .contains("must include at least one hunk line")
        });

    let explicit_empty_hunk =
        "*** Begin Patch\n*** Update File: docs/calculator-design.md\n@@ -1,1 +1,1\n*** End Patch";
    let empty_hunk_rejected = PatchParser::parse(explicit_empty_hunk)
        .err()
        .is_some_and(|error| {
            error
                .to_string()
                .contains("update hunk body cannot be empty")
        });

    hunkless_rejected && empty_hunk_rejected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunkless_update_patch_is_rejected_before_apply() {
        assert!(hunkless_update_patch_is_rejected_fixture_passes());
    }
}
