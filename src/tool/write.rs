use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;

use crate::edit::{
    ChangeSummary, CommittedFileMutation, EditSafety, FileChange, FileContentIdentity,
    FileReadStamp, FormatterExecutionOptions, ensure_edit_read_limit, path_for_change_storage,
};
use crate::error::ToolError;
use crate::session::{ChangeId, ChangeRepository};
use crate::tool::context::{ToolContext, ToolFormatterPlan};
use crate::tool::registry::Tool;
use crate::tool::write_support::{
    delete_file_conditionally, read_text_file_with_identity, to_summary,
    write_text_file_conditionally,
};
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard};

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
            effect: crate::tool::ToolEffectPolicy::mutation(),
            description: "Create a text file or replace the full contents of one file. Replacing an existing file requires a current complete-file edit baseline.",
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
        let formatter_plan = ToolFormatterPlan::resolve(ctx.config, ctx.workspace, &guarded)?;
        let mut risks = Vec::new();
        if PathGuard::targets_protected_workspace_authority(&ctx.workspace.root, &guarded) {
            risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
        }
        let permission_access = write_permission_access(formatter_plan.as_ref());
        let permission_details = formatter_plan
            .as_ref()
            .map(ToolFormatterPlan::permission_detail)
            .into_iter()
            .collect();
        let effect_admission = ctx
            .confirm_if_needed_with_details(
                permission_access,
                format!("Write full contents to {}", guarded.absolute),
                permission_details,
                vec![guarded.absolute.clone()],
                !guarded.inside_workspace && !guarded.trusted_external,
                risks,
            )
            .await?;
        ctx.run_mutation_fence.assert_owned().await?;

        let services = ctx.services.clone();
        let session_id = ctx.session.session.id;
        let tool_call_id = ctx.tool_call_id;
        let path = guarded.absolute.clone();
        let workspace_root = ctx.workspace.root.clone();
        let stored_path = path_for_change_storage(&path, &ctx.workspace.root);
        let locked_path = path.clone();
        let path_in_task = path.clone();
        let guarded_in_task = guarded.clone();
        let stored_path_in_task = stored_path.clone();
        let formatter_plan_in_task = formatter_plan.clone();
        let content = input.content;
        let turn_format = ctx.config.format.clone();
        let formatter_timeout_ms = ctx
            .config
            .shell
            .default_timeout_ms
            .min(ctx.config.shell.max_timeout_ms);
        let formatter_cancel = ctx.cancel.clone();
        let maximum_edit_read_bytes = ctx.config.file_guard.max_inline_read_bytes;
        let run_mutation_fence = ctx.run_mutation_fence.clone();
        let edit_safety = services.edit_safety.clone();
        let outcome = edit_safety
            .with_file_lock(&locked_path, async move {
                PathGuard::revalidate(&guarded_in_task)?;
                let (original, expected_identity) = if path_in_task.exists() {
                    let (original, identity) =
                        read_text_file_with_identity(&guarded_in_task, maximum_edit_read_bytes)?;
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
                    &turn_format,
                    &path_in_task,
                    original.as_deref(),
                    content,
                )?;
                let formatted = effect_admission
                    .format_if_planned(
                        &services.formatter,
                        formatter_plan_in_task.as_ref(),
                        normalized.clone(),
                        FormatterExecutionOptions {
                            timeout_ms: formatter_timeout_ms,
                            max_output_bytes: usize::try_from(maximum_edit_read_bytes)
                                .unwrap_or(usize::MAX),
                            cancel: formatter_cancel.clone(),
                        },
                    )
                    .await?;
                validate_final_write_content(&path_in_task, &formatted, maximum_edit_read_bytes)?;
                if original.as_deref() == Some(formatted.as_str()) {
                    let expected_identity = expected_identity.clone().ok_or_else(|| {
                        crate::error::EditError::CommitConflict {
                            path: path_in_task.clone(),
                        }
                    })?;
                    services.edit_safety.sync_file_mutations(
                        session_id,
                        &[CommittedFileMutation::present(
                            path_in_task.clone(),
                            expected_identity,
                        )],
                        maximum_edit_read_bytes,
                    )?;
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
                    maximum_edit_read_bytes,
                    &guarded_in_task,
                    original,
                    expected_identity,
                    formatted,
                    change,
                )
                .await
            })
            .await
            .map_err(ToolError::from)?;

        let (summary, change_ids) = match outcome {
            WriteExecutionOutcome::NoContent { path } => {
                return Ok(no_content_write_result(path));
            }
            WriteExecutionOutcome::Changed {
                summary,
                change_ids,
            } => (summary, change_ids),
        };

        Ok(ToolResult {
            title: format!("Wrote {}", summary.summary_line(Some(&ctx.workspace.root))),
            output_text: summary.summary_line(Some(&ctx.workspace.root)),
            metadata: json!({
                "changes": [json!({
                    "change_id": summary.change_id,
                    "kind": summary.kind,
                    "path_before": summary.path_before,
                    "path_after": summary.path_after
                })],
            }),
            truncated_output_path: None,
            recorded_changes: change_ids,
            change_summaries: vec![summary],
            _internal_file_lease: None,
        })
    }
}

fn write_permission_access(formatter_plan: Option<&ToolFormatterPlan>) -> AccessKind {
    if formatter_plan.is_some() {
        AccessKind::Shell
    } else {
        AccessKind::Edit
    }
}

enum WriteExecutionOutcome {
    NoContent {
        path: String,
    },
    Changed {
        summary: ChangeSummary,
        change_ids: Vec<ChangeId>,
    },
}

fn validate_final_write_content(
    path: &Utf8Path,
    formatted: &str,
    maximum_bytes: u64,
) -> Result<(), crate::error::EditError> {
    ensure_edit_read_limit(
        path,
        u64::try_from(formatted.len()).unwrap_or(u64::MAX),
        maximum_bytes,
    )
}

async fn commit_write_change(
    services: &crate::tool::context::ToolServices,
    run_mutation_fence: &crate::tool::context::RunMutationFence,
    session_id: crate::session::SessionId,
    maximum_edit_read_bytes: u64,
    guarded: &crate::workspace::GuardedPath,
    original: Option<String>,
    expected_identity: Option<FileContentIdentity>,
    formatted: String,
    change: FileChange,
) -> Result<WriteExecutionOutcome, ToolError> {
    let path = guarded.absolute.as_path();
    let baseline_snapshot = services
        .edit_safety
        .snapshot_path_stamps(session_id, &[path.to_path_buf()]);
    let rollback_state = match original {
        Some(value) => WriteRollbackState::Present(value),
        None => WriteRollbackState::Absent,
    };

    validate_write_commit_precondition(&services.edit_safety, path, expected_identity.as_ref())?;
    PathGuard::revalidate(guarded)?;
    run_mutation_fence.assert_owned().await?;
    let _effect_commit = run_mutation_fence.begin_effect_commit()?;

    let write_result =
        write_text_file_conditionally(guarded, &formatted, expected_identity.as_ref(), |file| {
            validate_write_temporary_file(guarded, file)
        });
    let committed_identity = write_result.map_err(ToolError::from)?;

    if let Err(error) = services.edit_safety.sync_file_mutations(
        session_id,
        &[CommittedFileMutation::present(
            path.to_path_buf(),
            committed_identity.clone(),
        )],
        maximum_edit_read_bytes,
    ) {
        rollback_write_commit(
            path,
            guarded,
            &rollback_state,
            &committed_identity,
            Some((&services.edit_safety, session_id, &baseline_snapshot)),
        )?;
        return Err(ToolError::from(error));
    }

    if let Err(error) = run_mutation_fence.assert_owned().await {
        rollback_write_commit(
            path,
            guarded,
            &rollback_state,
            &committed_identity,
            Some((&services.edit_safety, session_id, &baseline_snapshot)),
        )?;
        return Err(error);
    }

    match services
        .store
        .change_repo()
        .insert_changes(&[change.clone()])
        .await
    {
        Ok(change_ids) => {
            let summary = to_summary(&change);
            Ok(WriteExecutionOutcome::Changed {
                summary,
                change_ids,
            })
        }
        Err(error) => {
            rollback_write_commit(
                path,
                guarded,
                &rollback_state,
                &committed_identity,
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
    guarded: &crate::workspace::GuardedPath,
    rollback_state: &WriteRollbackState,
    committed_identity: &FileContentIdentity,
    baseline_snapshot: Option<(
        &crate::edit::EditSafety,
        crate::session::SessionId,
        &[(Utf8PathBuf, Option<FileReadStamp>)],
    )>,
) -> Result<(), ToolError> {
    restore_write_file_state(path, guarded, rollback_state, committed_identity)?;
    let mut rollback_errors = Vec::new();
    if let Some((edit_safety, session_id, snapshot)) = baseline_snapshot {
        if let Err(error) = edit_safety.restore_path_stamps(session_id, snapshot) {
            rollback_errors.push(error.to_string());
        }
    }
    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(ToolError::from(crate::error::EditError::RollbackFailed {
            operation: "write atomic commit".to_string(),
            details: rollback_errors.join("; "),
        }))
    }
}

fn restore_write_file_state(
    path: &Utf8Path,
    guarded: &crate::workspace::GuardedPath,
    state: &WriteRollbackState,
    committed_identity: &FileContentIdentity,
) -> Result<(), ToolError> {
    PathGuard::revalidate(guarded)?;
    EditSafety::default()
        .assert_path_unchanged(path, Some(committed_identity))
        .map_err(|_| {
            ToolError::from(crate::error::EditError::RollbackConflict {
                path: path.to_path_buf(),
            })
        })?;
    match state {
        WriteRollbackState::Absent => delete_file_conditionally(guarded, committed_identity)
            .map_err(|error| rollback_conflict(path, error))?,
        WriteRollbackState::Present(text) => {
            write_text_file_conditionally(guarded, text, Some(committed_identity), |file| {
                validate_write_temporary_file(guarded, file)
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

fn validate_write_temporary_file(
    guarded: &crate::workspace::GuardedPath,
    file: &std::fs::File,
) -> Result<(), crate::error::EditError> {
    PathGuard::validate_open_file_within_boundary(guarded, file)
        .map_err(|error| crate::error::EditError::Message(error.to_string()))
}

fn no_content_write_result(path: String) -> ToolResult {
    ToolResult {
        title: "No content changes made by write".to_string(),
        output_text: format!("write made no content changes to `{path}`"),
        metadata: json!({
            "no_content_change": true,
            "path": path,
            "success": false
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use std::time::Duration;

    use crate::cli::{ConfirmationPrompt, ReviewDecision};
    use crate::config::{AccessMode, FormatConfig, FormatterRule, NewlineStyle};
    use crate::edit::{
        ChangeTracker, EditSafety, Formatter, FormatterExecutionOptions, read_file_with_identity,
    };
    use crate::protocol::TurnInterruptionCause;
    use crate::runtime::RunControl;
    use crate::session::{
        AdmissionId, NewSession, ProjectId, ProjectRepository, SessionContext, SessionRepository,
        ToolCallId,
    };
    use crate::tool::context::{
        RunMutationFence, ToolContext, ToolEffectAdmission, ToolFormatterPlan, ToolServices,
        access_mode_allows_permission,
    };
    use crate::tool::registry::Tool;
    use crate::tool::truncate::ToolTruncator;
    use crate::workspace::{AccessKind, PathGuard, WorkspaceDiscovery};

    use super::{
        WriteRollbackState, WriteTool, restore_write_file_state, validate_final_write_content,
        validate_write_commit_precondition, write_permission_access,
    };

    #[derive(Default)]
    struct DenyPrompt {
        requests: Vec<crate::tool::PermissionRequest>,
    }

    impl ConfirmationPrompt for DenyPrompt {
        fn confirm(
            &mut self,
            request: &crate::tool::PermissionRequest,
        ) -> Result<ReviewDecision, crate::error::CliPromptError> {
            self.requests.push(request.clone());
            Ok(ReviewDecision::Denied)
        }
    }

    fn marker_format_config() -> FormatConfig {
        FormatConfig {
            default_newline: NewlineStyle::Lf,
            ensure_trailing_newline: true,
            commands: vec![FormatterRule {
                glob: "**/*.txt".to_string(),
                command: marker_wait_command(),
            }],
        }
    }

    fn marker_formatter_and_plan(
        root: &camino::Utf8Path,
        target: &camino::Utf8Path,
    ) -> (Formatter, ToolFormatterPlan) {
        let format = marker_format_config();
        let mut config = crate::config::ResolvedConfig::default();
        config.format = format.clone();
        let workspace = WorkspaceDiscovery::discover_fixed_root(root, &config).expect("workspace");
        let guarded = PathGuard::require_path(&workspace, target, AccessKind::Edit)
            .expect("guard formatter target");
        let plan = ToolFormatterPlan::resolve(&config, &workspace, &guarded)
            .expect("resolve formatter plan")
            .expect("matching formatter plan");
        (Formatter::new(format), plan)
    }

    fn formatter_options(control: &RunControl) -> FormatterExecutionOptions {
        FormatterExecutionOptions {
            timeout_ms: 30_000,
            max_output_bytes: 1_024,
            cancel: control.token(),
        }
    }

    #[cfg(windows)]
    fn marker_wait_command() -> Vec<String> {
        vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "Set-Content -LiteralPath 'formatter-started.marker' -Value 'started'; Start-Sleep -Seconds 30"
                .to_string(),
        ]
    }

    #[test]
    fn configured_formatter_promotes_write_to_shell_permission_in_every_access_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let target = root.join("output.txt");
        let (_, plan) = marker_formatter_and_plan(&root, &target);
        let request = crate::tool::PermissionRequest {
            access: write_permission_access(Some(&plan)),
            summary: "write with configured formatter".to_string(),
            details: vec![plan.permission_detail()],
            targets: vec![target],
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: None,
            agent_task_name: None,
        };

        assert_eq!(request.access, AccessKind::Shell);
        assert!(!access_mode_allows_permission(
            AccessMode::Default,
            &request
        ));
        assert!(!access_mode_allows_permission(
            AccessMode::FullAccess,
            &request
        ));
        assert!(request.details[0].contains("argv="));
        assert_eq!(write_permission_access(None), AccessKind::Edit);
    }

    #[tokio::test]
    async fn denied_full_access_formatter_spawns_no_process_and_mutates_no_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        std::fs::create_dir(&root).expect("workspace root");
        let storage_paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = crate::storage::SqliteStore::open(&storage_paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &root, "formatter denial", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "formatter denial".to_string(),
                cwd: root.clone(),
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::FullAccess,
            })
            .await
            .expect("session");
        let mut config = crate::config::ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::FullAccess;
        config.format = marker_format_config();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let session_context = SessionContext { session, workspace };
        let services = ToolServices {
            edit_safety: EditSafety::default(),
            formatter: Formatter::new(config.format.clone()),
            change_tracker: ChangeTracker,
            store: store.clone(),
            storage_paths,
            truncator: ToolTruncator,
            mcp: std::sync::Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
            skills: crate::skill::SkillsService::new(),
        };
        let control = RunControl::new();
        let mut prompt = DenyPrompt::default();
        let target = root.join("output.txt");
        let marker = root.join("formatter-started.marker");

        let error = WriteTool
            .execute(
                serde_json::json!({"path": "output.txt", "content": "content"}),
                ToolContext {
                    session: &session_context,
                    workspace: &session_context.workspace,
                    config: &config,
                    tool_call_id: ToolCallId::new(),
                    cancel: control.token(),
                    run_control: control.clone(),
                    run_mutation_fence: RunMutationFence::new(
                        store.session_repo(),
                        session_context.session.id,
                        AdmissionId::new(),
                        crate::protocol::TurnId::new(),
                        control,
                    ),
                    prompt: &mut prompt,
                    services: &services,
                    agent: None,
                },
            )
            .await
            .expect_err("denied formatter permission must stop write");

        assert!(matches!(
            error,
            crate::error::ToolError::PermissionDenied { .. }
        ));
        assert_eq!(prompt.requests.len(), 1);
        assert_eq!(prompt.requests[0].access, AccessKind::Shell);
        assert!(!marker.exists());
        assert!(!target.exists());
    }

    #[cfg(not(windows))]
    fn marker_wait_command() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf started > formatter-started.marker; sleep 30".to_string(),
        ]
    }

    #[tokio::test]
    async fn terminal_before_write_effect_admission_spawns_no_formatter_and_mutates_no_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let target = root.join("output.txt");
        let marker = root.join("formatter-started.marker");
        let control = RunControl::new();
        assert!(control.interrupt(TurnInterruptionCause::UserStop));
        let (formatter, plan) = marker_formatter_and_plan(&root, &target);

        let error = ToolEffectAdmission::new(control.clone())
            .format_if_planned(
                &formatter,
                Some(&plan),
                "content".to_string(),
                formatter_options(&control),
            )
            .await
            .expect_err("terminal producer must win before the formatter effect");

        assert!(matches!(error, crate::error::ToolError::RunInterrupted));
        assert!(!marker.exists());
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn stop_during_write_formatter_kills_it_before_file_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let target = root.join("output.txt");
        let marker = root.join("formatter-started.marker");
        let control = RunControl::new();
        let worker_control = control.clone();
        let worker_root = root.clone();
        let worker_target = target.clone();
        let worker = tokio::spawn(async move {
            let (formatter, plan) = marker_formatter_and_plan(&worker_root, &worker_target);
            ToolEffectAdmission::new(worker_control.clone())
                .format_if_planned(
                    &formatter,
                    Some(&plan),
                    "content".to_string(),
                    formatter_options(&worker_control),
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("formatter process must publish its start marker");
        assert!(control.interrupt(TurnInterruptionCause::UserStop));
        let error = tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .expect("formatter cancellation timeout")
            .expect("formatter worker")
            .expect_err("Stop must cancel the blocked formatter");

        assert!(error.to_string().contains("cancelled by user"));
        assert!(marker.exists());
        assert!(!target.exists());
    }

    #[test]
    fn commit_revalidation_preserves_same_size_external_rewrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "alpha").expect("seed file");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("capture identity");

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

    #[test]
    fn final_formatted_write_over_turn_limit_is_rejected_before_file_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("new.txt")).expect("utf8 path");

        let error = validate_final_write_content(&path, "12345", 4)
            .expect_err("oversized final formatted text must fail preflight");

        assert!(
            error
                .to_string()
                .contains("configured edit read limit of 4 bytes")
        );
        assert!(!path.exists());
    }

    #[test]
    fn rollback_cas_preserves_external_rewrite_after_agent_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "agent").expect("agent write");
        let workspace = WorkspaceDiscovery::discover_fixed_root(
            path.parent().expect("parent"),
            &crate::config::ResolvedConfig::default(),
        )
        .expect("test workspace");
        let guarded =
            PathGuard::require_path(&workspace, &path, crate::workspace::AccessKind::Edit)
                .expect("guarded path");
        let (_, committed_identity) =
            read_file_with_identity(&path, 1_024).expect("agent identity");
        std::fs::write(&path, "external").expect("external rewrite");

        let error = restore_write_file_state(
            &path,
            &guarded,
            &WriteRollbackState::Present("old".to_string()),
            &committed_identity,
        )
        .expect_err("rollback conflict must be explicit");

        assert!(error.to_string().contains("partially committed"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "external"
        );
    }
}
