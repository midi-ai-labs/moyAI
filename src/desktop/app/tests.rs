use super::{
    DESKTOP_RUNTIME_MAILBOX_CAPACITY, DesktopRenderer, DesktopSessionEventDelivery,
    HistoryExportRequestTarget, PendingPermission, PendingPermissionResolution,
    ProviderCatalogRequestTarget, RuntimeMessage, RuntimeMessageAsyncContract,
    SessionRefreshRequestTarget, SessionRuntimeListenerTarget, SteerSubmissionTarget,
    deliver_desktop_session_runtime_events, desktop_terminal_status_message,
    fallback_workspace_after_project_delete, finish_steer_operation_if_current,
    first_restorable_project_root, import_global_config_toml_to, normalize_image_attachment_path,
    notification_session_title, open_transcript_rows_to_markdown, provider_catalog_probe_config,
    publish_desktop_run_finished, resolve_pending_permission, run_completion_notification_body,
    run_terminal_event_notification_body, test_desktop_control_plane,
    transcript_markdown_file_name, unique_background_request_admission_open,
    write_markdown_export_atomic,
};
use crate::cli::{EventRenderer as _, ReviewDecision};
use crate::config::{ProviderProfile, ResolvedConfig};
use crate::desktop::async_ops::LatestRequestTracker;
use crate::desktop::models::DesktopTranscriptRowKind;
use crate::desktop::models::{DesktopFileChangeRow, DesktopSnapshot, DesktopTranscriptRow};
use crate::desktop::state::DesktopState;
use crate::protocol::{RuntimeEvent, RuntimeEventMsg, TurnId};
use crate::session::{ProjectId, ProjectRecord, RunEvent, RunSummary, SessionId};
use camino::{Utf8Path, Utf8PathBuf};
use std::sync::mpsc;

fn project_record(id: ProjectId, root_path: &str) -> ProjectRecord {
    ProjectRecord {
        id,
        root_path: root_path.into(),
        display_name: root_path.to_string(),
        vcs_kind: "none".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

#[test]
fn shared_session_projection_tracker_rejects_a_live_result_after_current_dispatch() {
    let target = SessionRefreshRequestTarget {
        workspace_root: "C:/workspace".into(),
        session_id: SessionId::new(),
    };
    let mut tracker = LatestRequestTracker::default();
    let stale_live_request = tracker.begin(target.clone());
    let current_request = tracker.begin(target.clone());

    assert!(tracker.finish_if_current(current_request, &target));
    assert!(!tracker.finish_if_current(stale_live_request, &target));
}

#[test]
fn stale_steer_target_does_not_consume_current_operation() {
    let current_session_id = SessionId::new();
    let mut state = DesktopState::new(
        DesktopSnapshot {
            workspace_path: "C:/current".to_string(),
            provider_label: String::new(),
            model_label: String::new(),
            command_rows: Vec::new(),
            project_rows: Vec::new(),
            selected_project_index: 0,
            session_rows: Vec::new(),
            chat_session_rows: Vec::new(),
            session_details: Vec::new(),
            selected_session_index: 0,
        },
        ResolvedConfig::default(),
    );
    state.app_state.current_session_id = Some(current_session_id);
    let operation_id = state.begin_steer_submission();
    let stale_target = SteerSubmissionTarget {
        operation_id,
        workspace_root: "C:/stale".into(),
        session_id: current_session_id,
        expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
    };

    assert!(!finish_steer_operation_if_current(
        &mut state,
        Utf8Path::new("C:/current"),
        &stale_target,
    ));
    assert!(state.steer_submission_pending());

    let current_target = SteerSubmissionTarget {
        operation_id,
        workspace_root: "C:/current".into(),
        session_id: current_session_id,
        expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
    };
    assert!(finish_steer_operation_if_current(
        &mut state,
        Utf8Path::new("C:/current"),
        &current_target,
    ));
    assert!(!state.steer_submission_pending());
    assert!(!finish_steer_operation_if_current(
        &mut state,
        Utf8Path::new("C:/current"),
        &current_target,
    ));
}

fn completed_run_summary(
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
) -> RunSummary {
    RunSummary::from_terminal(
        crate::session::SessionId::new(),
        crate::protocol::TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count,
            failed_tool_count,
            change_count,
            metrics: Default::default(),
        },
    )
}

fn interrupted_turn_event(
    session_id: crate::session::SessionId,
    cause: crate::protocol::TurnInterruptionCause,
) -> RunEvent {
    RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Interrupted { cause },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    }
}

#[tokio::test]
async fn fixed_workspace_load_does_not_rediscover_parent_git_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let outer = Utf8PathBuf::from_path_buf(temp.path().join("outer")).expect("utf8 outer");
    let quick_chat_root = outer.join("data/quick-chat-workspace");
    std::fs::create_dir_all(outer.join(".git")).expect("parent git marker");
    std::fs::create_dir_all(&quick_chat_root).expect("quick chat root");

    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("storage")).expect("utf8 storage");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);

    let app = crate::app::AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
        &quick_chat_root,
        store,
        ResolvedConfig::default(),
    )
    .await
    .expect("fixed workspace load");

    assert_eq!(app.workspace.root, quick_chat_root);
    assert_eq!(app.workspace.cwd, quick_chat_root);
    assert_eq!(app.workspace.vcs, crate::workspace::VcsKind::None);
}

#[test]
fn unique_background_request_admission_rejects_either_pending_owner() {
    assert!(unique_background_request_admission_open(false, false));
    assert!(!unique_background_request_admission_open(true, false));
    assert!(!unique_background_request_admission_open(false, true));
    assert!(!unique_background_request_admission_open(true, true));
}

#[test]
fn runtime_message_async_contract_classifies_representative_backflow_sources() {
    let provider_target = ProviderCatalogRequestTarget {
        base_url: "http://127.0.0.1:1234".to_string(),
        profile: ProviderProfile::LmStudio,
        api_key_env: None,
        config_generation: 1,
        selected_model_id: "selected-model".to_string(),
    };
    let provider_request_id = LatestRequestTracker::default().begin(provider_target.clone());
    let history_target = HistoryExportRequestTarget {
        workspace_authority_root: Utf8PathBuf::from("C:/workspace"),
        session_id: crate::session::SessionId::new(),
    };
    let history_request_id = LatestRequestTracker::default().begin(history_target.clone());
    assert_eq!(
        RuntimeMessage::HistoryExported {
            request_id: history_request_id,
            target: history_target,
            result: Ok(Utf8PathBuf::from("C:/workspace/history.md")),
        }
        .async_contract(),
        RuntimeMessageAsyncContract::BackgroundOperation
    );
    assert_eq!(
        RuntimeMessage::ModelCatalogLoaded {
            request_id: provider_request_id,
            target: provider_target,
            result: Ok(Vec::new()),
        }
        .async_contract(),
        RuntimeMessageAsyncContract::ProviderOperation
    );
    assert_eq!(
        {
            let target = SessionRefreshRequestTarget {
                workspace_root: Utf8PathBuf::from("C:/workspace"),
                session_id: crate::session::SessionId::new(),
            };
            let request_id = LatestRequestTracker::default().begin(target.clone());
            RuntimeMessage::LiveSessionRefreshed {
                request_id,
                target,
                result: Err("not loaded".to_string()),
            }
        }
        .async_contract(),
        RuntimeMessageAsyncContract::RunStream
    );
    assert_eq!(
        RuntimeMessage::Finished {
            run_generation: 1,
            result: Err("failed".to_string()),
        }
        .async_contract(),
        RuntimeMessageAsyncContract::TerminalRun
    );
}

#[test]
fn full_desktop_mailbox_keeps_the_canonical_terminal_retryable() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let target = SessionRuntimeListenerTarget {
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        session_id,
        turn_id,
    };
    let terminal_event = RuntimeEvent {
        id: crate::protocol::RuntimeEventId::new(),
        session_id,
        turn_id,
        sequence_no: 9,
        created_at_ms: 1,
        msg: RuntimeEventMsg::TurnTerminal {
            terminal: Box::new(crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 1,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        },
    };
    let (tx, rx) = mpsc::sync_channel(1);
    tx.try_send(RuntimeMessage::CanonicalSessionEvent {
        listener_generation: 1,
        target: target.clone(),
        event: RuntimeEvent {
            id: crate::protocol::RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 8,
            created_at_ms: 1,
            msg: RuntimeEventMsg::Warning {
                message: "occupy mailbox".to_string(),
            },
        },
    })
    .expect("fill mailbox");

    assert_eq!(
        deliver_desktop_session_runtime_events(
            &tx,
            1,
            &target,
            std::slice::from_ref(&terminal_event),
        ),
        DesktopSessionEventDelivery::MailboxFull
    );
    let _ = rx.recv().expect("release mailbox slot");
    assert_eq!(
        deliver_desktop_session_runtime_events(
            &tx,
            1,
            &target,
            std::slice::from_ref(&terminal_event),
        ),
        DesktopSessionEventDelivery::Terminal
    );
    assert!(matches!(
        rx.recv().expect("retried terminal"),
        RuntimeMessage::CanonicalSessionEvent { event, .. }
            if event.id == terminal_event.id
    ));
}

#[test]
fn stale_permission_answer_id_is_rejected_without_consuming_current_request() {
    let (response_tx, response_rx) = mpsc::channel();
    let run_control = crate::runtime::RunControl::new();
    let run_control_observer = run_control.clone();
    let mut pending = Some(PendingPermission {
        confirmation_id: 42,
        request: crate::tool::PermissionRequest {
            access: crate::workspace::AccessKind::Read,
            summary: "inspect the workspace".to_string(),
            details: Vec::new(),
            targets: Vec::new(),
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: None,
            agent_task_name: None,
        },
        responder: response_tx,
        run_control,
    });

    assert_eq!(
        resolve_pending_permission(&mut pending, 41, ReviewDecision::Approved),
        PendingPermissionResolution::NotCurrent
    );
    assert_eq!(
        pending.as_ref().map(|pending| pending.confirmation_id),
        Some(42)
    );
    assert!(matches!(
        response_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(run_control_observer.cause(), None);

    assert_eq!(
        resolve_pending_permission(&mut pending, 42, ReviewDecision::Abort),
        PendingPermissionResolution::Resolved
    );
    assert!(pending.is_none());
    assert_eq!(response_rx.try_recv(), Ok(ReviewDecision::Abort));
    assert_eq!(run_control_observer.cause(), None);
}

#[test]
fn terminal_status_message_uses_the_typed_interruption_cause() {
    let session_id = crate::session::SessionId::new();
    let approval_abort = interrupted_turn_event(
        session_id,
        crate::protocol::TurnInterruptionCause::ApprovalAborted,
    );
    assert_eq!(
        desktop_terminal_status_message(&approval_abort),
        Some(crate::tui::state::interruption_status_message(
            crate::protocol::TurnInterruptionCause::ApprovalAborted
        ))
    );

    let explicit_stop =
        interrupted_turn_event(session_id, crate::protocol::TurnInterruptionCause::UserStop);
    assert_eq!(
        desktop_terminal_status_message(&explicit_stop),
        Some(crate::tui::state::interruption_status_message(
            crate::protocol::TurnInterruptionCause::UserStop
        ))
    );
}

#[test]
fn provider_catalog_probe_uses_the_complete_connection_profile_input() {
    let mut config = ResolvedConfig::default();
    config.model.base_url = "http://old-provider:1234".to_string();
    config.model.provider_profile = ProviderProfile::OpenAiResponses;
    config.model.api_key_env = Some("OLD_PROVIDER_KEY".to_string());
    config.model.extra_headers.insert(
        "Authorization".to_string(),
        "Bearer must-not-cross-provider-targets".to_string(),
    );
    config.model.extra_body_json = Some(serde_json::json!({
        "api_key": "must-not-cross-provider-targets"
    }));

    let probe_config = provider_catalog_probe_config(
        config,
        "http://127.0.0.1:8110".to_string(),
        ProviderProfile::OpenAiCompatible,
        Some("OMLX_API_KEY".to_string()),
    );

    assert_eq!(probe_config.model.base_url, "http://127.0.0.1:8110");
    assert_eq!(
        probe_config.model.provider_profile,
        ProviderProfile::OpenAiCompatible
    );
    assert_eq!(
        probe_config.model.api_key_env.as_deref(),
        Some("OMLX_API_KEY")
    );
    assert!(probe_config.model.extra_headers.is_empty());
    assert_eq!(probe_config.model.extra_body_json, None);
}

#[test]
fn provider_catalog_probe_preserves_headers_but_not_legacy_generation_state_for_same_target() {
    let mut config = ResolvedConfig::default();
    config.model.base_url = "http://127.0.0.1:8110/v1/".to_string();
    config.model.provider_profile = ProviderProfile::OpenAiCompatible;
    config.model.extra_headers.insert(
        "X-Provider-Route".to_string(),
        "same-target-route".to_string(),
    );
    config.model.extra_body_json = Some(serde_json::json!({"num_ctx": 8192}));

    let same_target = provider_catalog_probe_config(
        config.clone(),
        "http://127.0.0.1:8110".to_string(),
        ProviderProfile::OpenAiCompatible,
        None,
    );
    assert_eq!(same_target.model.extra_headers, config.model.extra_headers);
    assert_eq!(same_target.model.extra_body_json, None);

    let changed_profile = provider_catalog_probe_config(
        config,
        "http://127.0.0.1:8110".to_string(),
        ProviderProfile::OpenAiResponses,
        None,
    );
    assert!(changed_profile.model.extra_headers.is_empty());
    assert_eq!(changed_profile.model.extra_body_json, None);
}

#[test]
fn image_attachment_normalization_allows_canonical_file_outside_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let image = temp.path().join("outside.png");
    std::fs::write(&image, b"png fixture").expect("image fixture");
    let workspace = Utf8PathBuf::from_path_buf(workspace).expect("utf8 workspace");
    let image = Utf8PathBuf::from_path_buf(image).expect("utf8 image");

    let normalized = normalize_image_attachment_path(&workspace, image.as_str())
        .expect("outside image should be explicitly attachable");

    assert!(normalized.is_absolute());
    assert_eq!(normalized.extension(), Some("png"));
    assert!(normalized.is_file());
}

#[test]
fn image_attachment_normalization_rejects_parent_traversal() {
    let error = normalize_image_attachment_path(Utf8Path::new("C:/workspace"), "../outside.png")
        .expect_err("parent traversal must be rejected before asset scoping");

    assert!(error.contains("parent-directory traversal"));
}

#[test]
fn image_attachment_normalization_preserves_nonexistent_path_diagnostic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 path");

    let error = normalize_image_attachment_path(&workspace, "missing-image.png")
        .expect_err("a missing image must remain rejected");

    assert!(error.contains("image path is not accessible"));
    assert!(!error.contains("the image path was not attached"));
}

#[test]
fn image_attachment_normalization_preserves_unsupported_extension_diagnostic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let unsupported = temp.path().join("not-an-image.txt");
    std::fs::write(&unsupported, b"not an image").expect("unsupported fixture");
    let workspace = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 path");
    let unsupported = Utf8PathBuf::from_path_buf(unsupported).expect("utf8 fixture");

    let error = normalize_image_attachment_path(&workspace, unsupported.as_str())
        .expect_err("an unsupported extension must remain rejected");

    assert_eq!(error, "unsupported image file extension: txt");
}

#[test]
fn project_delete_selects_only_non_deleted_remaining_project() {
    let deleted_id = ProjectId::new();
    let hidden_id = ProjectId::new();
    let kept_id = ProjectId::new();
    let hidden_root = camino::Utf8PathBuf::from("C:/workspace/hidden");
    let deleted_root = Utf8Path::new("C:/workspace/deleted");
    let projects = vec![
        project_record(deleted_id, "C:/workspace/deleted"),
        project_record(hidden_id, "C:/workspace/hidden"),
        project_record(kept_id, "C:/workspace/kept"),
    ];

    let selected =
        first_restorable_project_root(&projects, deleted_id, &[hidden_root], deleted_root)
            .expect("kept project should be restorable");

    assert_eq!(selected, camino::Utf8PathBuf::from("C:/workspace/kept"));
}

#[test]
fn project_delete_fallback_never_returns_deleted_or_hidden_root() {
    let deleted_root = Utf8Path::new("C:/workspace/deleted");
    let hidden_root = camino::Utf8PathBuf::from("C:/workspace/hidden");
    let data_dir = Utf8Path::new("C:/moyai-data");

    let fallback =
        fallback_workspace_after_project_delete(deleted_root, &[hidden_root.clone()], data_dir);

    assert_ne!(fallback.as_path(), deleted_root);
    assert_ne!(fallback, hidden_root);
}

#[cfg(windows)]
#[test]
fn markdown_export_atomic_supports_long_windows_paths_and_utf8_overwrite() {
    let temp = tempfile::tempdir().expect("export fixture directory");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_owned()).expect("UTF-8 fixture path");
    let mut parent = root.join("exports");
    while parent.as_str().encode_utf16().count() < 220 {
        parent.push("long-export-directory");
    }
    let path = parent.join(format!("{}-{}.md", "会話".repeat(24), SessionId::new()));
    assert!(path.as_str().encode_utf16().count() > 260);

    for markdown in [
        "# 会話の保存\n\n元の内容。🗾\n",
        "# 会話の更新\n\n新しい内容を UTF-8 で保存。✅\n",
    ] {
        write_markdown_export_atomic(&path, markdown).expect("atomic long-path export");
        assert_eq!(
            std::fs::read(path.as_std_path()).expect("exported bytes"),
            markdown.as_bytes()
        );
    }
    assert_eq!(
        std::fs::read_dir(parent.as_std_path())
            .expect("export directory")
            .count(),
        1,
        "successful overwrite must leave only the destination"
    );
}

#[test]
fn open_transcript_markdown_keeps_visible_rows_and_metadata() {
    let session_id = crate::session::SessionId::new();
    let rows = vec![
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::User,
            stable_history_identity: None,
            step: "01".to_string(),
            title: "Prompt".to_string(),
            body: "Older request.".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::Assistant,
            stable_history_identity: None,
            step: "02".to_string(),
            title: "Previous response".to_string(),
            body: "Earlier answer.".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::User,
            stable_history_identity: None,
            step: "03".to_string(),
            title: "Prompt".to_string(),
            body: "Create a report.".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::Assistant,
            stable_history_identity: None,
            step: "04".to_string(),
            title: "Response".to_string(),
            body: "Done.\nSaved files.".to_string(),
            file_changes: Vec::new(),
        },
    ];

    let markdown = open_transcript_rows_to_markdown(
        "Session #1",
        &Utf8PathBuf::from("C:/workspace"),
        session_id,
        "http://127.0.0.1:1234",
        "fixture-model",
        &rows,
        &[],
    );

    assert!(markdown.contains("# Session \\#1"));
    assert!(
        markdown.find("> Older request.").unwrap() < markdown.find("> Create a report.").unwrap(),
        "visible transcript export should preserve chronological user turn blocks"
    );
    assert!(markdown.contains("> Create a report."));
    assert!(
        markdown.find("Earlier answer.").unwrap() < markdown.find("> Create a report.").unwrap(),
        "assistant closeout for an earlier turn should not be folded under the latest user request"
    );
    assert!(markdown.contains("<details><summary>実行情報</summary>"));
    assert!(markdown.contains("- Provider: `http://127.0.0.1:1234`"));
    assert!(markdown.contains("Done.\nSaved files."));
    assert!(
        transcript_markdown_file_name("Session #1", session_id).ends_with(".md"),
        "transcript export should always use markdown extension"
    );
}

#[test]
fn open_transcript_markdown_preserves_visible_evidence() {
    let session_id = crate::session::SessionId::new();
    let rows = vec![
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::User,
            stable_history_identity: None,
            step: "01".to_string(),
            title: "Prompt".to_string(),
            body: "Create files.".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::Assistant,
            stable_history_identity: None,
            step: "02".to_string(),
            title: "Response".to_string(),
            body: "Now run this:\n<tool_call>\n<function=shell>\n</tool_call>".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::Diff,
            stable_history_identity: None,
            step: "03".to_string(),
            title: "File changes".to_string(),
            body: "Added README.md\nAdded __pycache__\\workflow.cpython-313.pyc".to_string(),
            file_changes: Vec::new(),
        },
    ];
    let changes = vec![DesktopFileChangeRow {
        label: "README.md".to_string(),
        path: "README.md".to_string(),
        kind: crate::session::ChangeKind::Add,
        action: "追加".to_string(),
        summary: "Added README.md".to_string(),
        tool_call_ids: vec![crate::session::ToolCallId::new()],
    }];

    let markdown = open_transcript_rows_to_markdown(
        "Case2",
        &Utf8PathBuf::from("C:/workspace"),
        session_id,
        "http://127.0.0.1:1234",
        "fixture-model",
        &rows,
        &changes,
    );

    assert!(markdown.contains("ファイル変更履歴"));
    assert!(markdown.contains("README.md"));
    assert!(markdown.contains("Now run this:"));
    assert!(markdown.contains("<tool_call>"));
    assert!(markdown.contains("__pycache__"));
    assert!(markdown.contains(".pyc"));
    assert!(
        !markdown.contains("完了しました。"),
        "Desktop open transcript Markdown export must not synthesize clean closeout text when visible assistant evidence contains a malformed pseudo tool-call"
    );
}

#[test]
fn open_transcript_markdown_uses_terminal_outcome_for_cancelled_turn() {
    let session_id = crate::session::SessionId::new();
    let rows = vec![
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::User,
            stable_history_identity: None,
            step: "01".to_string(),
            title: "Prompt".to_string(),
            body: "Update the implementation.".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::Assistant,
            stable_history_identity: None,
            step: "02".to_string(),
            title: "Response".to_string(),
            body: "テストの期待値を修正します。".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::WorkSummaryCancelled,
            stable_history_identity: None,
            step: "03".to_string(),
            title: "作業履歴 / 作業サマリ".to_string(),
            body: "### 作業サマリ\n- 結果: run cancelled by user".to_string(),
            file_changes: Vec::new(),
        },
    ];

    let markdown = open_transcript_rows_to_markdown(
        "Cancelled Session",
        &Utf8PathBuf::from("C:/workspace"),
        session_id,
        "http://127.0.0.1:1234",
        "fixture-model",
        &rows,
        &[],
    );

    assert!(markdown.contains("停止しました: run cancelled by user"));
    assert!(
        markdown.find("テストの期待値を修正します。").unwrap()
            < markdown
                .find("停止しました: run cancelled by user")
                .unwrap(),
        "intermediate assistant intent must remain folded before terminal outcome"
    );
}

#[test]
fn open_transcript_markdown_exports_a_prior_assistant_before_a_cancelled_later_turn_once() {
    let session_id = crate::session::SessionId::new();
    let rows = vec![
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::User,
            stable_history_identity: None,
            step: "01".to_string(),
            title: "Prompt".to_string(),
            body: "first request".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::WorkSummaryCompleted,
            stable_history_identity: None,
            step: "02".to_string(),
            title: "作業履歴 / 作業サマリ".to_string(),
            body: "### 作業サマリ\n- 結果: first turn completed".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::Assistant,
            stable_history_identity: None,
            step: "03".to_string(),
            title: "Response".to_string(),
            body: "FIRST_OK".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::User,
            stable_history_identity: None,
            step: "04".to_string(),
            title: "Prompt".to_string(),
            body: "second request".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: DesktopTranscriptRowKind::WorkSummaryCancelled,
            stable_history_identity: None,
            step: "05".to_string(),
            title: "作業履歴 / 作業サマリ".to_string(),
            body: "### 作業サマリ\n- 結果: second turn cancelled".to_string(),
            file_changes: Vec::new(),
        },
    ];

    let markdown = open_transcript_rows_to_markdown(
        "Two-turn session",
        &Utf8PathBuf::from("C:/workspace"),
        session_id,
        "http://127.0.0.1:1234",
        "fixture-model",
        &rows,
        &[],
    );

    let first_user = markdown.find("> first request").unwrap();
    let assistant = markdown.find("FIRST_OK").unwrap();
    let second_user = markdown.find("> second request").unwrap();
    let cancelled = markdown
        .find("停止しました: second turn cancelled")
        .unwrap();
    assert!(first_user < assistant && assistant < second_user && second_user < cancelled);
    assert_eq!(markdown.matches("FIRST_OK").count(), 1);
}

#[test]
fn completion_notification_body_summarizes_terminal_run() {
    let summary = completed_run_summary(3, 1, 2);

    let body = run_completion_notification_body("  vision GUI  ", &summary);

    assert!(body.contains("vision GUI が完了しました。"));
    assert!(body.contains("変更: 2件"));
    assert!(body.contains("ツール: 3件 / 失敗 1件"));
    assert_eq!(notification_session_title(""), "タスク");
}

#[test]
fn desktop_renderer_defers_state_completion_until_worker_settlement() {
    let (tx, rx) = mpsc::sync_channel(DESKTOP_RUNTIME_MAILBOX_CAPACITY);
    let (control, control_rx) = test_desktop_control_plane();
    let mut renderer = DesktopRenderer {
        runtime_tx: tx,
        bootstrap_control: control.clone(),
        run_generation: 12,
        notification_title: "test".to_string(),
        notified_terminal: false,
        control_result: None,
    };
    let summary = completed_run_summary(0, 0, 0);

    renderer.finish(&summary).expect("renderer finish");
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

    publish_desktop_run_finished(&control, 12, Ok(summary.clone()));
    assert!(matches!(
        control_rx.try_recv().expect("worker settlement"),
        RuntimeMessage::Finished {
            run_generation: 12,
            result: Ok(received),
        } if received.session_id() == summary.session_id()
    ));
}

#[test]
fn unbounded_control_plane_preserves_multiple_completions_in_order() {
    let (control, control_rx) = test_desktop_control_plane();
    let owner_session_id = SessionId::new();
    control
        .send(RuntimeMessage::SideChatFinished {
            owner_session_id,
            side_chat_id: "first".to_string(),
            run_generation: 1001,
            result: Err("first settlement".to_string()),
        })
        .expect("first settlement");
    control
        .send(RuntimeMessage::SideChatFinished {
            owner_session_id,
            side_chat_id: "second".to_string(),
            run_generation: 1002,
            result: Err("second settlement".to_string()),
        })
        .expect("second settlement");

    assert!(matches!(
        control_rx.recv().expect("first completion"),
        RuntimeMessage::SideChatFinished {
            run_generation: 1001,
            ..
        }
    ));
    assert!(matches!(
        control_rx.recv().expect("second completion"),
        RuntimeMessage::SideChatFinished {
            run_generation: 1002,
            ..
        }
    ));
    assert!(matches!(
        control_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn terminal_event_notification_body_uses_terminal_outcome() {
    let body = run_terminal_event_notification_body(
        "vision GUI",
        &interrupted_turn_event(
            crate::session::SessionId::new(),
            crate::protocol::TurnInterruptionCause::UserStop,
        ),
    )
    .expect("terminal event should produce a notification");

    assert_eq!(body, "vision GUI を停止しました: run stopped by user");
}

#[test]
fn config_import_accepts_toml_files_regardless_of_base_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    let text = "[model]\nmodel = \"renamed-config-model\"\n";
    let target = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");

    for file_name in ["config(1).toml", "config_202608.toml", "CONFIG_BACKUP.TOML"] {
        let path = Utf8PathBuf::from_path_buf(temp.path().join(file_name)).expect("utf8 path");
        std::fs::write(path.as_std_path(), text).expect("config fixture");
        std::fs::write(target.as_std_path(), "sentinel").expect("target fixture");

        import_global_config_toml_to(&path, &target)
            .expect("renamed TOML config should be imported");
        assert_eq!(std::fs::read_to_string(target.as_std_path()).unwrap(), text);
    }
}

#[test]
fn config_import_validates_canonical_and_legacy_response_timeout_together() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source =
        Utf8PathBuf::from_path_buf(temp.path().join("config(1).toml")).expect("utf8 source");
    let target = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
    let legacy = "[model]\nstream_idle_timeout_ms = 3600000\n";
    std::fs::write(source.as_std_path(), legacy).expect("legacy import fixture");

    import_global_config_toml_to(&source, &target)
        .expect("unambiguous legacy timeout should remain importable");
    assert_eq!(
        std::fs::read_to_string(target.as_std_path()).expect("imported config"),
        legacy
    );

    let sentinel = "[model]\nrequest_timeout_ms = 1800000\n";
    std::fs::write(target.as_std_path(), sentinel).expect("target sentinel");
    std::fs::write(
        source.as_std_path(),
        "[model]\nrequest_timeout_ms = 3600000\nstream_idle_timeout_ms = 1800000\n",
    )
    .expect("ambiguous import fixture");

    let error = import_global_config_toml_to(&source, &target)
        .expect_err("mismatched timeout aliases must not replace the active config");
    assert!(error.contains("model.request_timeout_ms"));
    assert!(error.contains("model.stream_idle_timeout_ms"));
    assert_eq!(
        std::fs::read_to_string(target.as_std_path()).expect("unchanged target"),
        sentinel
    );
}

#[test]
fn config_import_rejects_paths_without_a_toml_extension() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
    let sentinel = "[model]\nmodel = \"existing\"\n";

    for file_name in ["config_202608.txt", "config_backup", "config.toml.bak"] {
        let path = Utf8PathBuf::from_path_buf(temp.path().join(file_name)).expect("utf8 path");
        std::fs::write(path.as_std_path(), "[model]\nmodel = \"valid\"\n").expect("config fixture");
        std::fs::write(target.as_std_path(), sentinel).expect("target fixture");

        assert_eq!(
            import_global_config_toml_to(&path, &target),
            Err("select a .toml file".to_string())
        );
        assert_eq!(
            std::fs::read_to_string(target.as_std_path()).unwrap(),
            sentinel
        );
    }
}

#[test]
fn config_import_rejects_malformed_toml() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path =
        Utf8PathBuf::from_path_buf(temp.path().join("config_backup.toml")).expect("utf8 path");
    let target = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
    let sentinel = "[model]\nmodel = \"existing\"\n";
    let secret = "GLOBAL_IMPORT_PARSE_SECRET_SENTINEL";
    std::fs::write(
        path.as_std_path(),
        format!("[model]\nextra_headers = {{ Authorization = \"{secret}\", broken = }}\n"),
    )
    .expect("config fixture");
    std::fs::write(target.as_std_path(), sentinel).expect("target fixture");

    let error =
        import_global_config_toml_to(&path, &target).expect_err("malformed TOML must fail closed");
    assert_eq!(
        error,
        "the selected TOML config is invalid or does not match the current config schema"
    );
    assert!(!error.contains(secret));
    assert!(!error.contains("Authorization"));
    assert_eq!(
        std::fs::read_to_string(target.as_std_path()).unwrap(),
        sentinel
    );
}

#[test]
fn config_import_rejects_semantically_invalid_config_before_replacing_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source =
        Utf8PathBuf::from_path_buf(temp.path().join("config(1).toml")).expect("utf8 source");
    let target = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
    let sentinel = "[model]\nmodel = \"existing\"\n";
    std::fs::write(
        source.as_std_path(),
        "[workspace]\nprotected_paths = [\"relative/path\"]\n",
    )
    .expect("semantic-invalid config fixture");
    std::fs::write(target.as_std_path(), sentinel).expect("target fixture");

    let error = import_global_config_toml_to(&source, &target)
        .expect_err("semantic-invalid config must not be imported");

    assert!(error.contains("workspace.protected_paths"));
    assert!(error.contains("absolute path"));
    assert_eq!(
        std::fs::read_to_string(target.as_std_path()).unwrap(),
        sentinel
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_toast_script_quotes_notification_text() {
    let script = super::windows_toast_script("moy'AI", "done & \"quoted\"");

    assert!(script.contains("'moy''AI'"));
    assert!(script.contains("'done & \"quoted\"'"));
    assert!(script.contains("ShowBalloonTip"));
}
