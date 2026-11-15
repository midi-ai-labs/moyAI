use super::*;

async fn build_test_app(root: &Utf8Path, store: crate::storage::StoreBundle) -> App {
    build_test_app_with_config(root, store, ResolvedConfig::default()).await
}

async fn build_test_app_with_config(
    root: &Utf8Path,
    store: crate::storage::StoreBundle,
    config: ResolvedConfig,
) -> App {
    AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(root, store, config)
        .await
        .expect("app")
}

#[tokio::test]
async fn startup_preferences_restore_the_same_conversation_after_nested_git_init() {
    use crate::session::{NewSession, SessionRepository as _};
    let (_temp, root, controller) = empty_access_test_controller().await;
    let nested = root.join("nested");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "ongoing work".into(),
            cwd: nested.clone(),
            model: "test".into(),
            base_url: "http://127.0.0.1:9".into(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .unwrap();
    std::fs::create_dir_all(nested.join(".git")).unwrap();
    let discovered = AppBootstrap::rebuild_for_directory_with_process_runtime(
        &nested,
        controller.app.process_runtime.clone(),
    )
    .await
    .unwrap();
    assert_ne!(discovered.workspace.project_id, session.project_id);
    let preferences = DesktopPreferences {
        last_workspace: Some(nested.clone()),
        last_session_id: Some(session.id),
        ..Default::default()
    };
    let args = DesktopArgs {
        directory: None,
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let restored = DesktopController::new_with_preferences_and_persistence(
        discovered.clone(),
        args.clone(),
        preferences.clone(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(restored.app.workspace.project_id, session.project_id);
    assert_eq!(restored.app.workspace.cwd, nested);
    assert_eq!(
        restored.state.app_state.current_session_id,
        Some(session.id)
    );
    assert_eq!(restored.preferences.last_session_id, Some(session.id));
    let selected =
        &restored.state.snapshot.project_rows[restored.state.snapshot.selected_project_index];
    assert_eq!(selected.path, root.as_str());

    // A deliberate directory choice or --continue-last follows its own
    // current workspace, even when saved preferences point elsewhere.
    for explicit in [
        DesktopArgs {
            directory: Some(nested.clone()),
            ..args.clone()
        },
        DesktopArgs {
            continue_last: true,
            ..args
        },
    ] {
        let launched = DesktopController::new_with_preferences_and_persistence(
            discovered.clone(),
            explicit,
            preferences.clone(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            launched.app.workspace.project_id,
            discovered.workspace.project_id
        );
        assert_ne!(
            launched.state.app_state.current_session_id,
            Some(session.id)
        );
    }
}

#[tokio::test]
async fn startup_preferences_restore_legacy_workspace_and_report_missing_folders() {
    let (_temp, root, controller) = empty_access_test_controller().await;
    let saved = root.join("saved");
    std::fs::create_dir_all(&saved).unwrap();
    let args = DesktopArgs {
        directory: None,
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let prefs = DesktopPreferences {
        last_workspace: Some(saved.clone()),
        ..Default::default()
    };
    let restored = DesktopController::new_with_preferences_and_persistence(
        controller.app.clone(),
        args.clone(),
        prefs.clone(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(restored.app.workspace.cwd, saved);
    assert_eq!(restored.preferences.last_session_id, None);
    let missing = DesktopPreferences {
        last_workspace: Some(root.join("missing")),
        ..prefs.clone()
    };
    let restored = DesktopController::new_with_preferences_and_persistence(
        controller.app.clone(),
        args.clone(),
        missing,
        false,
    )
    .await
    .unwrap();
    assert_eq!(restored.app.workspace.cwd, root);
    assert!(
        restored
            .state
            .app_state
            .status_message
            .as_deref()
            .unwrap()
            .contains("前回の作業フォルダーを開けませんでした")
    );
    assert!(!root.join("missing").exists());
    let deleted = DesktopPreferences {
        deleted_project_roots: vec![saved],
        ..prefs
    };
    let restored = DesktopController::new_with_preferences_and_persistence(
        controller.app.clone(),
        args,
        deleted,
        false,
    )
    .await
    .unwrap();
    assert_eq!(restored.app.workspace.cwd, root);
}

#[tokio::test]
async fn startup_restores_nested_workspace_and_exports_history_inside_its_authority() {
    use crate::session::{NewSession, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let project_root =
        Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
    let selected_directory = project_root.join("bbb");
    std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
    std::fs::create_dir_all(selected_directory.join("ccc")).expect("selected directory");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&project_root, store).await;
    let session = app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: app.workspace.project_id,
            title: "nested workspace".to_string(),
            cwd: selected_directory.clone(),
            model: "session-saved-model".to_string(),
            base_url: "http://127.0.0.1:4555/v1".to_string(),
            access_mode: app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    app.store
        .protocol_event_store()
        .seed_history_item_for_test(&crate::protocol::HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            scope: crate::protocol::HistoryScope::Turn {
                turn_id: crate::protocol::TurnId::new(),
            },
            sequence_no: 1,
            created_at_ms: 1,
            payload: crate::protocol::HistoryItemPayload::UserTurn {
                content: vec![crate::protocol::ContentPart::Text {
                    text: "export from the selected nested workspace".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        })
        .expect("history item");
    let args = DesktopArgs {
        directory: Some(project_root.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };

    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");

    assert_eq!(controller.app.workspace.root, project_root);
    assert_eq!(controller.app.workspace.cwd, selected_directory);
    assert_eq!(
        controller.state.app_state.current_session_id,
        Some(session.id)
    );
    controller
        .state
        .provider_config
        .effective_config
        .model
        .model = "current-settings-model".to_string();
    controller
        .state
        .provider_config
        .effective_config
        .model
        .base_url = "http://127.0.0.1:4666/v1".to_string();

    let export_title = controller
        .state
        .snapshot
        .session_rows
        .iter()
        .find(|row| row.session_id == session.id)
        .expect("session row")
        .label
        .clone();
    let file_name = history_markdown_file_name(&export_title, session.id);
    let expected_export = selected_directory
        .join(".moyai")
        .join("history-exports")
        .join(&file_name);
    let ancestor_export = project_root
        .join(".moyai")
        .join("history-exports")
        .join(file_name);
    controller.export_history_markdown_auto(session.id);
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if controller.state.can_export_history() && expected_export.is_file() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(
        expected_export.is_file(),
        "expected {expected_export}; status: {:?}",
        controller.state.app_state.status_message
    );
    assert!(
        !ancestor_export.exists(),
        "automatic export must not escape to the ancestor project root"
    );
    assert!(
        controller
            .state
            .app_state
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains(expected_export.as_str()))
    );
    let canonical_history =
        std::fs::read_to_string(&expected_export).expect("canonical history markdown");
    assert!(canonical_history.contains("Model: session-saved-model"));
    assert!(canonical_history.contains("Base URL: http://127.0.0.1:4555/v1"));

    let transcript_file_name = transcript_markdown_file_name(&session.title, session.id);
    let expected_transcript = selected_directory
        .join(".moyai")
        .join("transcript-exports")
        .join(&transcript_file_name);
    let ancestor_transcript = project_root
        .join(".moyai")
        .join("transcript-exports")
        .join(transcript_file_name);
    controller.export_open_transcript_markdown_auto();
    let transcript =
        std::fs::read_to_string(&expected_transcript).expect("exported transcript markdown");
    assert!(
        !ancestor_transcript.exists(),
        "automatic transcript export must not escape to the ancestor project root"
    );
    assert!(transcript.contains(selected_directory.as_str()));
    assert!(transcript.contains("Provider: `http://127.0.0.1:4555/v1`"));
    assert!(transcript.contains("Model: `session-saved-model`"));
    assert!(!transcript.contains("http://127.0.0.1:4666/v1"));
    assert!(!transcript.contains("current-settings-model"));
}

#[tokio::test]
async fn session_navigation_restores_the_sessions_nested_workspace_cwd() {
    use crate::session::{NewSession, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let project_root =
        Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
    let selected_directory = project_root.join("bbb");
    std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
    std::fs::create_dir_all(selected_directory.join("ccc")).expect("selected directory");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&project_root, store).await;
    let args = DesktopArgs {
        directory: Some(project_root.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "nested workspace".to_string(),
            cwd: selected_directory.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");

    let request_id = controller.state.begin_session_load(session.id);
    controller.spawn_session_load(session.id, SessionLoadReason::UserSelection, request_id);
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if !controller.state.navigation_loading() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!controller.state.navigation_loading());
    assert_eq!(controller.app.workspace.root, project_root);
    assert_eq!(controller.app.workspace.cwd, selected_directory);
    assert_eq!(
        controller.state.app_state.current_session_id,
        Some(session.id)
    );
}

#[tokio::test]
async fn picked_project_directory_starts_new_chat_without_restoring_another_cwd() {
    use crate::session::{NewSession, SessionRepository as _};

    let (_temp, project_root, mut controller) = empty_access_test_controller().await;
    std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
    let old_cwd = project_root.join("old-workspace");
    let selected_cwd = project_root.join("selected-workspace");
    std::fs::create_dir_all(&old_cwd).expect("old workspace");
    std::fs::create_dir_all(&selected_cwd).expect("selected workspace");
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "Existing nested chat".to_string(),
            cwd: old_cwd.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("old session");
    // The old directory need not still exist when a user selects a new one.
    std::fs::remove_dir(&old_cwd).expect("remove unused old workspace");

    controller.open_picked_project_directory(selected_cwd.clone());
    for _ in 0..400 {
        controller.drain_runtime_messages();
        if !controller.state.navigation_loading() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!controller.state.navigation_loading());
    assert_eq!(controller.app.workspace.root, project_root);
    assert_eq!(controller.app.workspace.cwd, selected_cwd);
    assert_eq!(controller.app.workspace.authority_root(), selected_cwd);
    assert_eq!(controller.state.app_state.current_session_id, None);
    assert_eq!(controller.state.selected_session_id(), None);
    assert!(
        controller
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session.id)
    );
    let preserved = controller
        .app
        .session_service
        .get_session(session.id)
        .await
        .expect("preserved session");
    assert_eq!(preserved.cwd, old_cwd);
    assert_eq!(preserved.project_id, controller.app.workspace.project_id);

    controller.refresh_snapshot();
    for _ in 0..400 {
        controller.drain_runtime_messages();
        if !controller.snapshot_requests.is_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!controller.snapshot_requests.is_pending());
    assert_eq!(controller.state.selected_session_id(), None);
    assert_eq!(controller.state.app_state.current_session_id, None);
    let mut late_none_snapshot = controller.state.snapshot.clone();
    late_none_snapshot.workspace_path = old_cwd.to_string();

    let created = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "Created in selected workspace".into(),
            cwd: selected_cwd.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("created session");
    let turn_id = TurnId::new();
    let prompt = "new workspace transcript";
    controller
        .app
        .store
        .protocol_event_store()
        .seed_history_item_for_test(&crate::protocol::HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: created.id,
            scope: crate::protocol::HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 1,
            payload: crate::protocol::HistoryItemPayload::UserTurn {
                content: vec![crate::protocol::ContentPart::Text {
                    text: prompt.into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        })
        .expect("created session history");
    let reply = "new workspace reply";
    for (index, payload) in [
        crate::protocol::TurnItemPayload::UserMessage {
            text: prompt.into(),
        },
        crate::protocol::TurnItemPayload::AgentMessage { text: reply.into() },
    ]
    .into_iter()
    .enumerate()
    {
        controller
            .app
            .store
            .protocol_event_store()
            .seed_turn_item_for_test(&crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id: created.id,
                turn_id,
                source_item_id: None,
                sequence_no: index as i64 + 2,
                payload,
            })
            .expect("created session transcript item");
    }
    let terminal = crate::session::DurableTurnTerminal {
        outcome: crate::protocol::TurnTerminalOutcome::Completed,
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    controller.run_lifecycle.begin(1, RunControl::new());
    for event in [
        RunEvent::SessionStarted {
            session_id: created.id,
            title: created.title.clone(),
        },
        RunEvent::UserTurnStored {
            session_id: created.id,
            turn: Box::new(UserTurn {
                turn_id,
                items: vec![UserInputItem::Text {
                    text: prompt.into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            }),
        },
        RunEvent::TurnTerminal {
            session_id: created.id,
            terminal: Box::new(terminal.clone()),
        },
    ] {
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: 1,
                event,
            })
            .expect("run event");
    }
    controller
        .runtime_tx
        .send(RuntimeMessage::Finished {
            run_generation: 1,
            result: Ok(RunSummary::from_terminal(created.id, turn_id, terminal)),
        })
        .expect("worker finished");
    for _ in 0..400 {
        controller.drain_runtime_messages();
        if !controller.run_lifecycle.root_is_active()
            && !controller.snapshot_requests.is_pending()
            && !controller.session_projection_refresh_requests.is_pending()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!controller.run_lifecycle.root_is_active());
    assert!(!controller.snapshot_requests.is_pending());
    assert!(!controller.session_projection_refresh_requests.is_pending());
    assert_eq!(controller.state.selected_session_id(), Some(created.id));
    assert_eq!(
        controller.state.app_state.current_session_id,
        Some(created.id)
    );
    assert_eq!(controller.app.workspace.cwd, selected_cwd);

    // A delayed fresh-draft result must not select the previous chat after admission.
    let none_target = SnapshotRequestTarget {
        workspace_root: project_root.clone(),
        selected_session_id: None,
    };
    let none_request = controller.snapshot_requests.begin(none_target.clone());
    controller.state.begin_snapshot_refresh();
    controller
        .runtime_tx
        .send(RuntimeMessage::SnapshotLoaded {
            request_id: none_request,
            target: none_target,
            result: Ok(late_none_snapshot.clone()),
        })
        .expect("late draft snapshot");
    controller.drain_runtime_messages();
    assert_eq!(controller.state.selected_session_id(), Some(created.id));
    assert_eq!(
        controller.state.snapshot.workspace_path,
        selected_cwd.as_str()
    );

    let current_target = SnapshotRequestTarget {
        workspace_root: project_root.clone(),
        selected_session_id: Some(created.id),
    };
    let stale_request = controller.snapshot_requests.begin(current_target.clone());
    let latest_request = controller.snapshot_requests.begin(current_target.clone());
    controller.state.begin_snapshot_refresh();
    controller
        .runtime_tx
        .send(RuntimeMessage::SnapshotLoaded {
            request_id: stale_request,
            target: current_target.clone(),
            result: Ok(late_none_snapshot.clone()),
        })
        .expect("superseded snapshot");
    controller.drain_runtime_messages();
    assert!(
        controller
            .snapshot_requests
            .is_current(latest_request, &current_target)
    );
    assert_eq!(controller.state.selected_session_id(), Some(created.id));
    assert_eq!(
        controller.state.snapshot.workspace_path,
        selected_cwd.as_str()
    );
    controller
        .runtime_tx
        .send(RuntimeMessage::SnapshotLoaded {
            request_id: latest_request,
            target: current_target,
            result: Ok(controller.state.snapshot.clone()),
        })
        .expect("latest snapshot");
    controller.drain_runtime_messages();

    let other_workspace_target = SnapshotRequestTarget {
        workspace_root: project_root.join("another-project"),
        selected_session_id: Some(created.id),
    };
    let other_request = controller
        .snapshot_requests
        .begin(other_workspace_target.clone());
    controller.state.begin_snapshot_refresh();
    controller
        .runtime_tx
        .send(RuntimeMessage::SnapshotLoaded {
            request_id: other_request,
            target: other_workspace_target,
            result: Ok(late_none_snapshot),
        })
        .expect("other workspace snapshot");
    controller.drain_runtime_messages();
    assert_eq!(controller.state.selected_session_id(), Some(created.id));
    assert_eq!(
        controller.state.snapshot.workspace_path,
        selected_cwd.as_str()
    );

    controller.export_open_transcript_markdown_auto();
    let export_path = selected_cwd
        .join(".moyai")
        .join("transcript-exports")
        .join(transcript_markdown_file_name(&created.title, created.id));
    let exported = std::fs::read_to_string(&export_path).expect("new chat transcript export");
    assert!(
        exported.contains(prompt),
        "exported transcript:\n{exported}"
    );
    assert!(exported.contains(reply), "exported transcript:\n{exported}");
    assert!(exported.contains(&created.id.to_string()));
    assert!(!exported.contains(&session.id.to_string()));
    assert_eq!(
        controller
            .app
            .session_service
            .get_session(session.id)
            .await
            .expect("old chat retained")
            .cwd,
        old_cwd
    );
}

#[tokio::test]
async fn stale_picked_project_completion_cannot_replace_a_newer_directory_selection() {
    let (_temp, project_root, mut controller) = empty_access_test_controller().await;
    let stale_cwd = project_root.join("stale");
    let latest_cwd = project_root.join("latest");
    std::fs::create_dir_all(&stale_cwd).expect("stale directory");
    std::fs::create_dir_all(&latest_cwd).expect("latest directory");
    let stale_app = AppBootstrap::rebuild_for_directory_with_process_runtime(
        &stale_cwd,
        controller.app.process_runtime.clone(),
    )
    .await
    .expect("stale app");
    let stale_snapshot = load_snapshot_for_selection(&stale_app, None)
        .await
        .expect("snapshot");
    let stale_request = controller
        .state
        .begin_new_project_session_workspace_load(stale_cwd.clone());
    // Replacing DesktopState resets the navigation sequence. A pending
    // completion from its previous owner can then have the same request ID.
    controller.state.navigation = Default::default();
    let latest_request = controller
        .state
        .begin_new_project_session_workspace_load(latest_cwd);
    assert_eq!(stale_request, latest_request);

    controller.apply_new_project_workspace_switched_message(
        stale_request,
        stale_cwd.clone(),
        Ok(WorkspaceLoadResult {
            app: stale_app,
            snapshot: stale_snapshot,
        }),
    );

    assert_eq!(controller.app.workspace.cwd, project_root);
    assert!(controller.state.is_current_navigation(latest_request));
    assert!(controller.state.navigation_loading());
    controller.apply_new_project_workspace_switched_message(
        stale_request,
        stale_cwd.clone(),
        Err("old directory load failed".to_string()),
    );
    assert!(controller.state.is_current_navigation(latest_request));
    assert!(controller.state.navigation_loading());

    controller.state.navigation = Default::default();
    let ordinary_request = controller
        .state
        .begin_workspace_load(stale_cwd.clone(), None);
    assert_eq!(ordinary_request, stale_request);
    controller.apply_new_project_workspace_switched_message(
        stale_request,
        stale_cwd,
        Err("old new-chat load failed".to_string()),
    );
    assert!(controller.state.is_current_navigation(ordinary_request));
    assert!(controller.state.navigation_loading());
}

#[tokio::test]
async fn nested_first_session_submission_adopts_the_authority_owned_composer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project_root =
        Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
    let selected_directory = project_root.join("bbb");
    std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
    std::fs::create_dir_all(&selected_directory).expect("selected directory");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let root_app = build_test_app(&project_root, store).await;
    let app = AppBootstrap::rebuild_for_directory_with_process_runtime(
        &selected_directory,
        root_app.process_runtime.clone(),
    )
    .await
    .expect("nested app");
    let args = DesktopArgs {
        directory: Some(selected_directory.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let next_image = selected_directory.join("next-request.png");
    let owner_generation = controller.state.composer.owner_generation();
    let run_generation = 41;
    controller
        .state
        .composer
        .image_attachment_paths
        .push(next_image.clone());
    controller.pending_root_submission = Some(PendingRootSubmission {
        run_generation,
        owner_workspace_path: controller.root_submission_owner_workspace_path(),
        owner_session_id: None,
        prompt_dispatch: crate::session::PromptDispatchPart::raw("first nested run"),
        image_paths: Vec::new(),
        prompt_review_to_cancel: None,
    });
    let created_session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(created_session_id);

    assert_eq!(
        controller.root_submission_owner_workspace_path(),
        selected_directory
    );
    assert!(controller.commit_pending_root_submission(run_generation, None));
    assert_eq!(
        controller.state.composer.image_attachment_paths,
        vec![next_image]
    );
    assert_eq!(
        controller.state.composer.owner_generation(),
        owner_generation,
        "adopting the first durable session must not reset the authority-owned draft"
    );
    assert!(controller.state.composer.is_owned_by(
        controller.app.workspace.authority_root().as_str(),
        Some(created_session_id)
    ));
}

#[tokio::test]
async fn nested_workspace_preference_preserves_the_selected_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project_root =
        Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
    let selected_directory = project_root.join("bbb");
    std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
    std::fs::create_dir_all(&selected_directory).expect("selected directory");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let root_app = build_test_app(&project_root, store).await;
    let app = AppBootstrap::rebuild_for_directory_with_process_runtime(
        &selected_directory,
        root_app.process_runtime.clone(),
    )
    .await
    .expect("nested app");
    let args = DesktopArgs {
        directory: Some(selected_directory.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");

    assert_eq!(controller.app.workspace.root, project_root);
    assert_eq!(
        controller.app.workspace.authority_root(),
        selected_directory
    );
    assert_eq!(
        controller.workspace_path_for_preferences(),
        Some(selected_directory)
    );
}

#[tokio::test]
async fn workspace_replacement_keeps_the_process_publish_owner_and_fixed_target() {
    use crate::session::{NewSession, SessionRepository as _};

    let (temp, project_root, mut controller) = empty_access_test_controller().await;
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "Published main".to_string(),
            cwd: controller.app.workspace.cwd.clone(),
            model: "unused-by-MCP".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            access_mode: crate::config::AccessMode::Default,
            provider_connection: None,
        })
        .await
        .unwrap();
    let target = crate::mcp_publish::PublishTarget::Project {
        project_id: session.project_id,
        workspace_root: session.cwd,
    };
    let publish = crate::mcp_publish::PublishService::new(
        Utf8PathBuf::from_path_buf(temp.path().join("application-config/mcp-publish.json"))
            .unwrap(),
        controller.app.store.clone(),
        controller.app.config.clone(),
    );
    let initial = publish.refresh().await.unwrap();
    let saved = publish
        .save(
            None,
            crate::mcp_publish::PublishDraft {
                label: "Fixed workspace read".to_string(),
                mode: crate::mcp_publish::PublishMode::ReadTools {},
                tls: None,
                bind: "127.0.0.1:7332".parse().unwrap(),
                target: target.clone(),
                tools: vec![crate::tool::ToolName::Read],
                max_concurrent_calls: 1,
                background: crate::mcp_publish::PublishBackgroundPolicy::StopWhenWindowCloses,
            },
            &initial.revision,
            &initial.generation,
        )
        .await
        .unwrap();
    let profile_id = saved.profiles[0].profile.id;
    controller.state.mcp_publish = Some(publish.clone());
    assert!(controller.next_web_state().unwrap().mcp_publish.is_some());

    let replacement_root =
        Utf8PathBuf::from_path_buf(temp.path().join("different-workspace")).unwrap();
    std::fs::create_dir(&replacement_root).unwrap();
    let replacement_app = AppBootstrap::rebuild_for_directory_with_process_runtime(
        &replacement_root,
        controller.app.process_runtime.clone(),
    )
    .await
    .unwrap();
    let snapshot = load_snapshot_for_selection(&replacement_app, None)
        .await
        .unwrap();
    assert!(controller.replace_workspace_from_load(WorkspaceLoadResult {
        app: replacement_app,
        snapshot
    }));
    assert_ne!(controller.app.workspace.root, project_root);
    let projected = controller
        .next_web_state()
        .unwrap()
        .mcp_publish
        .expect("process publication remains visible after project switch");
    assert_eq!(projected.profiles[0].profile.id, profile_id);
    assert_eq!(projected.profiles[0].profile.target, target);

    // A command on the original managed service must still update the current
    // Desktop projection: copying configuration into a new owner is insufficient.
    let receipt = publish
        .issue_token(profile_id, &projected.revision, &projected.generation)
        .await
        .unwrap();
    let current = controller.next_web_state().unwrap().mcp_publish.unwrap();
    assert_eq!(current.revision, receipt.projection.revision);
    assert!(current.profiles[0].credential_configured);
    assert_eq!(current.profiles[0].profile.target, target);
    assert!(publish.shutdown().await);
}

#[tokio::test]
async fn stale_session_navigation_cannot_replace_a_newer_workspace_with_reused_request_id() {
    let (temp, project_root, mut controller) = empty_access_test_controller().await;
    controller.state.hub_connection = Some(crate::hub::HubConnection::new(
        crate::hub::HubSettingsStore::new(
            Utf8PathBuf::from_path_buf(temp.path().join("hub-settings.json")).unwrap(),
        ),
    ));
    let session_id = SessionId::new();
    let stale_request_id = controller.state.begin_session_load(session_id);
    let stale_target = SessionLoadRequestTarget {
        workspace_root: controller.app.workspace.root.clone(),
        workspace_cwd: controller.app.workspace.cwd.clone(),
        project_id: controller.app.workspace.project_id,
        session_id,
    };
    let stale_loaded = loaded_test_session(
        &controller,
        &project_root,
        session_id,
        SessionStatus::Idle,
        None,
    );

    std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
    let newer_cwd = project_root.join("nested");
    std::fs::create_dir_all(&newer_cwd).expect("newer cwd");
    let newer_app = AppBootstrap::rebuild_for_directory_with_process_runtime(
        &newer_cwd,
        controller.app.process_runtime.clone(),
    )
    .await
    .expect("newer app");
    let newer_snapshot = load_snapshot_for_selection(&newer_app, None)
        .await
        .expect("newer snapshot");
    controller.replace_workspace_from_load(WorkspaceLoadResult {
        app: newer_app,
        snapshot: newer_snapshot,
    });
    let reused_request_id = controller.state.begin_session_load(session_id);
    assert_eq!(reused_request_id, stale_request_id);

    controller.apply_session_loaded_message(
        stale_request_id,
        stale_target,
        SessionLoadReason::UserSelection,
        Ok(SessionNavigationLoadResult {
            workspace: None,
            snapshot: None,
            loaded: stale_loaded,
        }),
    );

    assert_eq!(controller.app.workspace.cwd, newer_cwd);
    assert!(controller.state.navigation_loading());
    assert_eq!(controller.state.app_state.current_session_id, None);
    assert!(controller.next_web_state().unwrap().hub.is_some());
}

#[tokio::test]
async fn initial_setup_purpose_restores_team_without_replacing_direct_config() {
    let (_temp, project_root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .begin_startup(false, global_config_path().ok(), &project_root);
    let before = serde_json::to_value(controller.state.global_config()).unwrap();
    controller
        .choose_initial_setup_purpose(DesktopOnboardingIntent::Team)
        .unwrap();
    assert!(controller.state.view.hub_project_open);
    assert_eq!(
        serde_json::to_value(controller.state.global_config()).unwrap(),
        before
    );
    let preferences = controller.preferences.clone();
    let app = build_test_app(&project_root, controller.app.store.clone()).await;
    let args = DesktopArgs {
        directory: Some(project_root),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let restarted =
        DesktopController::new_with_preferences_and_persistence(app, args, preferences, false)
            .await
            .unwrap();
    assert!(restarted.state.view.hub_project_open);
    assert_eq!(
        restarted.state.view.overlay,
        super::super::state::DesktopOverlay::None
    );
    assert_eq!(
        restarted.preferences.onboarding_intent,
        Some(DesktopOnboardingIntent::Team)
    );
}

#[tokio::test]
async fn execution_setup_opens_and_restores_hub_before_manual_ai_configuration() {
    let (_temp, project_root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .begin_startup(false, global_config_path().ok(), &project_root);
    let before = serde_json::to_value(controller.state.global_config()).unwrap();
    controller
        .choose_initial_setup_purpose(DesktopOnboardingIntent::Execution)
        .unwrap();
    controller.state.apply_startup_overlay();
    let chosen = controller.next_web_state().unwrap();
    assert_eq!(chosen.overlay, "hub");
    assert!(chosen.hub_project_open);
    assert!(
        chosen.startup.initial_setup_required,
        "choosing execution must not pretend the Hub config was saved"
    );
    assert_eq!(
        serde_json::to_value(controller.state.global_config()).unwrap(),
        before
    );
    let preferences = controller.preferences.clone();
    let app = build_test_app(&project_root, controller.app.store.clone()).await;
    let mut restarted = DesktopController::new_with_preferences_and_persistence(
        app,
        DesktopArgs {
            directory: Some(project_root),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        },
        preferences,
        false,
    )
    .await
    .unwrap();
    restarted.state.apply_startup_overlay();
    let restored = restarted.next_web_state().unwrap();
    assert_eq!(restored.overlay, "hub");
    assert!(restored.hub_project_open && restored.startup.initial_setup_required);
    assert_eq!(
        restarted.preferences.onboarding_intent,
        Some(DesktopOnboardingIntent::Execution)
    );
    assert_eq!(
        serde_json::to_value(restarted.state.global_config()).unwrap(),
        before
    );
}

#[test]
fn initial_setup_saved_connection_survives_preferences_failure() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../project_sandbox/desktop-preferences-persistence-tests");
    std::fs::create_dir_all(&base).unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let config = temp.path().join("config.toml");
    let blocked_preferences = temp.path().join("preferences-file-is-a-directory");
    std::fs::write(&config, "").unwrap();
    std::fs::create_dir(&blocked_preferences).unwrap();
    // Use a dedicated child so process-wide config/preferences paths never
    // alter another test or the real Windows user's profile.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::app::command_projection_owner_tests::initial_setup_saved_connection_preferences_failure_fixture",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MOYAI_CONFIG_PATH", &config)
            .env("MOYAI_DESKTOP_PREFS_PATH", &blocked_preferences)
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "Dedicated child process for isolated config/preferences persistence failure"]
async fn initial_setup_saved_connection_preferences_failure_fixture() {
    let config = Utf8PathBuf::from(std::env::var("MOYAI_CONFIG_PATH").unwrap());
    let preferences = Utf8PathBuf::from(std::env::var("MOYAI_DESKTOP_PREFS_PATH").unwrap());
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../project_sandbox/desktop-preferences-persistence-tests")
        .canonicalize()
        .unwrap();
    assert!(
        config
            .as_std_path()
            .canonicalize()
            .unwrap()
            .starts_with(&base)
    );
    assert_eq!(
        preferences,
        config.with_file_name("preferences-file-is-a-directory")
    );
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    controller.preferences.onboarding_intent = Some(DesktopOnboardingIntent::Personal);
    controller
        .state
        .begin_startup(false, global_config_path().ok(), &root);
    controller.persist_preferences_to_disk = true;
    let service = crate::device_network::DeviceNetworkService::for_workspace(
        root.join("device-network"),
        root.clone(),
        controller.app.store.clone(),
        controller.state.global_config().clone(),
    )
    .await
    .unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::default();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let shared = crate::device_network::SharedHubConfig {
        hub_url: "https://127.0.0.1:19471".into(),
        ca_certificate_pem: params.self_signed(&key).unwrap().pem(),
    };
    let before = service.projection_now();
    let prepared = service
        .prepare_configuration(shared.clone(), &before.revision, &before.generation)
        .await
        .unwrap();
    let result = service
        .commit_configuration(prepared, |saved| {
            controller
                .save_device_network_config(saved, true)
                .map_err(|_| crate::device_network::DeviceError::Storage)
        })
        .await;
    let persisted: toml::Value =
        toml::from_str(&std::fs::read_to_string(global_config_path().unwrap()).unwrap()).unwrap();
    assert_eq!(
        persisted["device_network"]["hub_url"].as_str(),
        Some(shared.hub_url.as_str())
    );
    assert_eq!(controller.state.global_config().device_network, shared);
    assert!(
        result.is_ok(),
        "the connection is already persisted; preferences failure cannot turn its commit into an error: {result:?}"
    );
    assert_eq!(service.projection_now().hub_url, shared.hub_url);
    assert!(
        controller
            .finish_initial_setup_after_device_network_persist()
            .is_err()
    );
    assert_eq!(
        controller.preferences.onboarding_intent,
        Some(DesktopOnboardingIntent::Personal)
    );
    assert!(controller.state.startup.requires_initial_setup());
    assert_eq!(service.projection_now().hub_url, shared.hub_url);
    // The failed completion is retryable without rolling back or importing
    // the already committed connection again.
    std::fs::remove_dir(&preferences).unwrap();
    controller
        .finish_initial_setup_after_device_network_persist()
        .unwrap();
    assert!(!controller.state.startup.requires_initial_setup());
    assert_eq!(controller.preferences.onboarding_intent, None);
    assert_eq!(service.projection_now().hub_url, shared.hub_url);
}

#[tokio::test]
async fn initial_setup_purpose_restores_personal_review_after_config_creation() {
    let (_temp, project_root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .begin_startup(false, global_config_path().ok(), &project_root);
    controller
        .choose_initial_setup_purpose(DesktopOnboardingIntent::Personal)
        .unwrap();
    let app = build_test_app(&project_root, controller.app.store.clone()).await;
    let args = DesktopArgs {
        directory: Some(project_root),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let restarted = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        controller.preferences.clone(),
        false,
    )
    .await
    .unwrap();
    assert!(restarted.state.startup.requires_initial_setup());
    assert_eq!(
        restarted.state.view.overlay,
        super::super::state::DesktopOverlay::InitialSetup
    );
    assert_eq!(
        restarted.state.startup.onboarding_intent,
        Some(DesktopOnboardingIntent::Personal)
    );
}

#[tokio::test]
async fn initial_setup_latch_rejects_direct_and_async_workspace_owner_replacement() {
    let (temp, project_root, mut controller) = empty_access_test_controller().await;
    let replacement_root = Utf8PathBuf::from_path_buf(temp.path().join("replacement-workspace"))
        .expect("utf8 replacement root");
    std::fs::create_dir_all(replacement_root.join(".git")).expect("replacement workspace");
    let replacement_app = AppBootstrap::rebuild_for_directory_with_process_runtime(
        &replacement_root,
        controller.app.process_runtime.clone(),
    )
    .await
    .expect("replacement app");
    let replacement_snapshot = load_snapshot_for_selection(&replacement_app, None)
        .await
        .expect("replacement snapshot");
    let original_workspace_root = controller.app.workspace.root.clone();
    let original_workspace_cwd = controller.app.workspace.cwd.clone();
    let original_project_id = controller.app.workspace.project_id;
    let original_snapshot_workspace = controller.state.snapshot.workspace_path.clone();
    let original_workspace_input = controller.state.workspace_input.clone();

    controller.state.begin_startup(false, None, &project_root);
    let setup_generation = controller.state.startup.setup_generation;
    let setup_reason = controller.state.startup.initial_setup_reason;
    assert!(controller.state.startup.requires_initial_setup());
    assert!(controller.state.view.startup_overlay_forced);

    assert!(!controller.switch_workspace_to(replacement_root.to_string()));
    assert_eq!(controller.state.workspace_input, original_workspace_input);
    assert!(!controller.select_project_and_open(0));
    assert!(!controller.start_project_session(0));
    assert!(!controller.delete_project(original_project_id));
    assert!(!controller.select_session_and_open(0));
    assert!(!controller.start_quick_chat());
    assert!(!controller.create_project_from_picker());
    assert!(!controller.state.navigation_loading());
    assert!(!controller.state.background_mutation_pending());

    let request_id = controller
        .state
        .begin_workspace_load(replacement_root.clone(), None);
    controller.apply_workspace_switched_message(
        request_id,
        Ok(WorkspaceLoadResult {
            app: replacement_app.clone(),
            snapshot: replacement_snapshot.clone(),
        }),
    );
    assert!(
        !controller.state.navigation_loading(),
        "a rejected completion must settle its stale navigation operation"
    );

    let operation_id = controller.state.begin_project_delete_mutation();
    controller
        .runtime_tx
        .send(RuntimeMessage::ProjectDeleted {
            target: ProjectDeleteRequestTarget {
                workspace_root: original_workspace_root.clone(),
                owner_project_id: original_project_id,
                project_id: original_project_id,
                project_root: original_workspace_root.clone(),
                operation_id,
            },
            result: Ok(WorkspaceLoadResult {
                app: replacement_app,
                snapshot: replacement_snapshot,
            }),
        })
        .expect("project completion");
    controller.drain_runtime_messages();

    assert_eq!(controller.app.workspace.root, original_workspace_root);
    assert_eq!(controller.app.workspace.cwd, original_workspace_cwd);
    assert_eq!(controller.app.workspace.project_id, original_project_id);
    assert_eq!(
        controller.state.snapshot.workspace_path,
        original_snapshot_workspace
    );
    assert_eq!(controller.state.startup.setup_generation, setup_generation);
    assert_eq!(controller.state.startup.initial_setup_reason, setup_reason);
    assert!(controller.state.startup.requires_initial_setup());
    assert!(controller.state.view.startup_overlay_forced);
    assert!(!controller.state.navigation_loading());
    assert!(!controller.state.background_mutation_pending());
    assert!(
        !controller
            .preferences
            .is_project_deleted(&original_workspace_root),
        "a rejected current-project completion must not mutate remembered owners"
    );
}

#[tokio::test]
async fn desktop_cold_start_sends_no_provider_or_docling_requests() {
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking probe listener");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let request_count = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let request_count = request_count.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        request_count.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));
                        let mut request = [0_u8; 4096];
                        let _ = stream.read(&mut request);
                        let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                            );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })
    };

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.model.base_url = endpoint.clone();
    config.model.model = "cold-start-model".to_string();
    config.docling.enabled = true;
    config.docling.base_url = endpoint;
    let app = build_test_app_with_config(&root, store, config).await;
    let args = DesktopArgs {
        directory: Some(root),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };

    let _controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    std::thread::sleep(std::time::Duration::from_millis(250));
    stop.store(true, Ordering::SeqCst);
    server.join().expect("probe server");

    assert_eq!(request_count.load(Ordering::SeqCst), 0);
}

#[test]
fn terminal_event_moves_root_run_to_finalizing_without_admitting_steer() {
    let cancel = RunControl::new();
    let mut lifecycle = DesktopRunLifecycle::default();
    lifecycle.begin(7, cancel);
    assert!(lifecycle.owns(7));
    assert!(!lifecycle.owns(6));
    assert_eq!(lifecycle.root_generation(), Some(7));
    assert!(lifecycle.can_steer_root());
    lifecycle.observe_terminal_event();

    assert!(lifecycle.root_is_active());
    assert!(lifecycle.root_is_finalizing());
    assert!(!lifecycle.can_steer_root());
    assert_eq!(
        navigation_admission_blocker(false, false, false, lifecycle.root_is_finalizing(),),
        Some("the current run is finalizing")
    );
    lifecycle.finish_root();
    assert!(!lifecycle.root_is_active());
    assert_eq!(lifecycle.root_generation(), None);
    assert_eq!(
        navigation_admission_blocker(false, false, false, lifecycle.root_is_finalizing(),),
        None
    );
}

#[test]
fn root_stop_attempt_is_single_flight_and_stale_completion_cannot_release_a_new_attempt() {
    let mut lifecycle = DesktopRunLifecycle::default();
    let root_control = RunControl::new();
    lifecycle.begin(12, root_control.clone());

    let first = match lifecycle.begin_stop_attempt(12) {
        DesktopRootStopAttemptAdmission::Acquired {
            attempt,
            run_control,
        } => {
            assert!(run_control.same_owner(&root_control));
            attempt
        }
        _ => panic!("the exact generation must acquire its first Stop coordinator"),
    };
    assert!(matches!(
        lifecycle.begin_stop_attempt(12),
        DesktopRootStopAttemptAdmission::AlreadyPending
    ));
    assert!(!lifecycle.finish_stop_attempt(DesktopRootStopAttempt {
        generation: 11,
        attempt_id: first.attempt_id,
    }));
    assert!(lifecycle.finish_stop_attempt(first));

    let second = match lifecycle.begin_stop_attempt(12) {
        DesktopRootStopAttemptAdmission::Acquired { attempt, .. } => attempt,
        _ => panic!("the exact generation must admit a retry after settlement"),
    };
    assert_ne!(second.attempt_id, first.attempt_id);
    assert!(!lifecycle.finish_stop_attempt(first));
    assert!(matches!(
        lifecycle.begin_stop_attempt(12),
        DesktopRootStopAttemptAdmission::AlreadyPending
    ));
    assert!(lifecycle.finish_stop_attempt(second));

    lifecycle.begin(13, RunControl::new());
    assert!(matches!(
        lifecycle.begin_stop_attempt(12),
        DesktopRootStopAttemptAdmission::NotOwned
    ));
}

#[test]
fn settings_effective_config_commit_updates_the_next_turn_owner() {
    let mut state = DesktopState::new(
        DesktopSnapshot {
            workspace_path: "C:/workspace".to_string(),
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
    let mut candidate = ResolvedConfig::default();
    candidate.permissions.access_mode = crate::config::AccessMode::FullAccess;

    commit_effective_config(&mut state, candidate);

    assert_eq!(
        state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        crate::config::AccessMode::FullAccess
    );
}

#[tokio::test]
async fn settings_reject_invalid_constraints_before_effective_config_commit() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let generation = controller.state.provider_config.config_generation;

    for (field, value) in [
        (ConfigField::ContextWindow, "0"),
        (ConfigField::MaxParallelPredictions, "0"),
        (ConfigField::MultiAgentMaxAgents, "0"),
        (ConfigField::MultiAgentMaxModelRequests, "0"),
        (ConfigField::Temperature, "NaN"),
        (ConfigField::TopP, "inf"),
        (ConfigField::PresencePenalty, "-inf"),
        (ConfigField::FrequencyPenalty, "NaN"),
    ] {
        let original = field.editor_value(&controller.state.provider_config.effective_config);

        assert!(
            !controller.apply_session_config(vec![(field.label().to_string(), value.to_string(),)]),
            "{} must be rejected",
            field.label(),
        );
        assert_eq!(
            field.editor_value(&controller.state.provider_config.effective_config),
            original,
            "{} changed despite the rejected commit",
            field.label(),
        );
        assert_eq!(
            controller.state.provider_config.config_generation, generation,
            "a rejected config must not advance the owner generation",
        );
    }
}

#[tokio::test]
async fn failed_global_save_result_keeps_disk_commit_consumers_unchanged() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let original_app_model = controller.app.config.model.model.clone();
    let original_effective_model = controller
        .state
        .provider_config
        .effective_config
        .model
        .model
        .clone();
    let original_generation = controller.state.provider_config.config_generation;

    let error = controller
        .commit_global_config_save_result(Err("simulated preflight failure".to_string()))
        .expect_err("failed persistence must not commit runtime owners");

    assert_eq!(error, "simulated preflight failure");
    assert_eq!(controller.app.config.model.model, original_app_model);
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .model
            .model,
        original_effective_model
    );
    assert_eq!(
        controller.state.provider_config.config_generation,
        original_generation
    );
}

#[tokio::test]
async fn latest_docling_readiness_completion_wins_across_config_generation() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let mut config = controller.state.provider_config.effective_config.clone();
    config.docling.enabled = true;
    config.docling.base_url = "http://127.0.0.1:8123".to_string();
    controller.reset_effective_config_without_network(config.clone());
    let stale_target = DoclingReadinessRequestTarget {
        owner: DoclingReadinessConfigOwner::EffectiveConfig,
        base_url: config.docling.base_url.clone(),
        config_generation: controller.state.provider_config.config_generation,
    };
    let stale_request_id = controller
        .docling_readiness_requests
        .begin(stale_target.clone());
    controller
        .state
        .begin_docling_readiness_check(format!("{}/ready", stale_target.base_url));

    config.docling.base_url = "http://127.0.0.1:8124".to_string();
    controller.reset_effective_config_without_network(config);
    let latest_target = DoclingReadinessRequestTarget {
        owner: DoclingReadinessConfigOwner::EffectiveConfig,
        base_url: controller
            .state
            .provider_config
            .effective_config
            .docling
            .base_url
            .clone(),
        config_generation: controller.state.provider_config.config_generation,
    };
    let latest_request_id = controller
        .docling_readiness_requests
        .begin(latest_target.clone());
    controller
        .state
        .begin_docling_readiness_check(format!("{}/ready", latest_target.base_url));
    controller
        .runtime_tx
        .send(RuntimeMessage::DoclingReadinessChecked {
            request_id: stale_request_id,
            target: stale_target,
            result: Ok(crate::docling::DoclingReadinessResult {
                endpoint: "http://127.0.0.1:8123/ready".to_string(),
                http_status: 503,
                ready: false,
            }),
        })
        .expect("stale readiness completion");
    controller
        .runtime_tx
        .send(RuntimeMessage::DoclingReadinessChecked {
            request_id: latest_request_id,
            target: latest_target,
            result: Ok(crate::docling::DoclingReadinessResult {
                endpoint: "http://127.0.0.1:8124/ready".to_string(),
                http_status: 204,
                ready: true,
            }),
        })
        .expect("latest readiness completion");
    controller.drain_runtime_messages();

    assert_eq!(
        controller.state.docling_readiness.status,
        super::super::state::DesktopDoclingReadinessStatus::Ready
    );
    assert_eq!(
        controller.state.docling_readiness.endpoint,
        "http://127.0.0.1:8124/ready"
    );
    assert_eq!(controller.state.docling_readiness.http_status, Some(204));
    assert!(!controller.state.docling_readiness_check_pending());
}

#[tokio::test]
async fn initial_setup_draft_docling_completion_does_not_require_an_effective_endpoint_match() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let mut effective = controller.state.provider_config.effective_config.clone();
    effective.docling.enabled = false;
    effective.docling.base_url = "http://127.0.0.1:8123".to_string();
    controller.reset_effective_config_without_network(effective.clone());
    controller.state.begin_startup(false, None, &root);
    assert!(controller.state.startup.requires_initial_setup());

    let target = DoclingReadinessRequestTarget {
        owner: DoclingReadinessConfigOwner::InitialSetupDraft,
        base_url: "http://127.0.0.1:8124".to_string(),
        config_generation: controller.state.provider_config.config_generation,
    };
    let request_id = controller.docling_readiness_requests.begin(target.clone());
    controller
        .state
        .begin_docling_readiness_check("http://127.0.0.1:8124/ready".to_string());
    controller
        .runtime_tx
        .send(RuntimeMessage::DoclingReadinessChecked {
            request_id,
            target,
            result: Ok(crate::docling::DoclingReadinessResult {
                endpoint: "http://127.0.0.1:8124/ready".to_string(),
                http_status: 204,
                ready: true,
            }),
        })
        .expect("draft readiness completion");
    controller.drain_runtime_messages();

    assert_eq!(
        controller.state.docling_readiness.status,
        super::super::state::DesktopDoclingReadinessStatus::Ready
    );
    assert_eq!(
        controller.state.docling_readiness.endpoint,
        "http://127.0.0.1:8124/ready"
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .docling
            .enabled,
        effective.docling.enabled
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .docling
            .base_url,
        effective.docling.base_url
    );
}

#[tokio::test]
async fn global_provider_candidate_does_not_mix_root_effective_overrides() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let mut global = controller.state.global_config().clone();
    global.model.base_url = "http://127.0.0.1:1234".to_string();
    global.model.model = "global-model".to_string();
    global.model.context_window = 32_768;
    global.model.max_output_tokens = 2_048;
    global.model.supports_tools = false;
    global.model.temperature = Some(0.2);
    global.permissions.access_mode = crate::config::AccessMode::Default;
    controller.app.config = global.clone();
    controller.state.replace_global_config(global.clone());

    let mut root_effective = global.clone();
    root_effective.model.base_url = "http://127.0.0.1:5678".to_string();
    root_effective.model.model = "root-model".to_string();
    root_effective.model.context_window = 131_072;
    root_effective.model.max_output_tokens = 8_192;
    root_effective.model.supports_tools = true;
    root_effective.model.temperature = Some(0.9);
    root_effective.permissions.access_mode = crate::config::AccessMode::FullAccess;
    controller.reset_effective_config_without_network(root_effective);
    assert!(controller.state.show_provider_editor());

    let candidate = controller
        .apply_provider_selection_to_global_config()
        .expect("global provider candidate");

    assert_eq!(candidate.model.base_url, global.model.base_url);
    assert_eq!(candidate.model.model, global.model.model);
    assert_eq!(candidate.model.context_window, global.model.context_window);
    assert_eq!(
        candidate.model.max_output_tokens,
        crate::config::DEFAULT_MODEL_MAX_OUTPUT_TOKENS
    );
    assert_eq!(candidate.model.supports_tools, global.model.supports_tools);
    assert_eq!(candidate.model.temperature, None);
    assert_eq!(
        candidate.permissions.access_mode,
        global.permissions.access_mode
    );
}

#[test]
fn initial_setup_import_loads_a_complete_validated_draft_without_a_state_owner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path =
        Utf8PathBuf::from_path_buf(temp.path().join("import.toml")).expect("UTF-8 import path");
    std::fs::write(
            path.as_std_path(),
            "[model]\nmodel = \"imported-model\"\nbase_url = \"http://127.0.0.1:1234\"\nextra_headers = { Authorization = \"INITIAL_SETUP_FILE_IMPORT_SECRET\" }\n",
        )
        .expect("write valid import");

    let config = DesktopController::load_initial_setup_config_toml_path(&path)
        .expect("validated initial-setup draft");
    assert_eq!(
        config
            .model
            .extra_headers
            .get("Authorization")
            .map(String::as_str),
        Some("INITIAL_SETUP_FILE_IMPORT_SECRET"),
        "the Rust-only staged owner receives the exact imported secret"
    );
    let public_secret = ConfigField::ExtraHeadersJson.public_value(&config);
    assert!(public_secret.sensitive);
    assert!(public_secret.configured);
    assert!(public_secret.value.is_empty());
    let values = ConfigField::ALL
        .into_iter()
        .filter(|field| !field.is_host_owned_generation())
        .map(|field| (field.label().to_string(), field.public_value(&config).value))
        .collect::<Vec<_>>();

    let current_gui_fields = ConfigField::ALL
        .into_iter()
        .filter(|field| !field.is_host_owned_generation())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), current_gui_fields.len());
    for field in current_gui_fields {
        assert!(
            values.iter().any(|(key, _)| key == field.label()),
            "all current GUI ConfigField values must be projected: {}",
            field.label()
        );
    }
    for field in ConfigField::ALL
        .into_iter()
        .filter(|field| field.is_host_owned_generation())
    {
        assert!(
            values.iter().all(|(key, _)| key != field.label()),
            "host-owned generation field must not enter the imported GUI draft: {}",
            field.label()
        );
    }

    std::fs::write(
        path.as_std_path(),
        "[model]\nmodel = \"imported-model\"\n\n[unknown]\nflag = true\n",
    )
    .expect("write invalid import");
    assert!(
        DesktopController::load_initial_setup_config_toml_path(&path).is_err(),
        "unknown current-schema sections must fail closed"
    );
}

#[test]
fn initial_setup_malformed_toml_error_never_echoes_source_secrets() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = Utf8PathBuf::from_path_buf(temp.path().join("secret-import.toml"))
        .expect("UTF-8 import path");
    let secret = "INITIAL_SETUP_PARSE_SECRET_SENTINEL";
    std::fs::write(
        path.as_std_path(),
        format!("[model]\nextra_headers = {{ Authorization = \"{secret}\", broken = }}\n"),
    )
    .expect("write malformed import");

    let error = DesktopController::load_initial_setup_config_toml_path(&path)
        .expect_err("malformed TOML must fail closed");
    assert_eq!(
        error,
        "the selected TOML config is invalid or does not match the current config schema"
    );
    assert!(!error.contains(secret));
    assert!(!error.contains(path.as_str()));
    assert!(!error.contains("Authorization"));
}

#[tokio::test]
async fn initial_setup_import_secrets_stay_in_one_generation_fenced_rust_owner() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let first_secret = "INITIAL_IMPORT_SECRET_ONE";
    let second_secret = "INITIAL_IMPORT_SECRET_TWO";
    let prompt_marker = "INITIAL_IMPORT_MAIN_PROMPT_MARKER";
    let mut first = ResolvedConfig::default();
    first.model.system_prompt = prompt_marker.to_string();
    first
        .model
        .extra_headers
        .insert("X-Import-Secret".to_string(), first_secret.to_string());
    first
        .docling
        .headers
        .insert("Authorization".to_string(), first_secret.to_string());
    let (first_generation, public_values) = controller
        .stage_initial_setup_config_import(first)
        .expect("stage first import");
    for key in [
        ConfigField::ExtraHeadersJson,
        ConfigField::DoclingHeadersJson,
    ] {
        let public = public_values
            .iter()
            .find(|value| value.key == key.label())
            .expect("sensitive public field");
        assert!(public.sensitive);
        assert!(public.configured);
        assert!(public.text.is_empty());
    }
    let prompt_public = public_values
        .iter()
        .find(|value| value.key == ConfigField::SystemPrompt.label())
        .expect("main system prompt public field");
    assert_eq!(prompt_public.text, prompt_marker);
    let public_debug = format!("{public_values:?}");
    assert!(!public_debug.contains(first_secret));
    assert!(!public_debug.contains(prompt_marker));
    assert!(public_debug.contains("text_chars"));

    let mut second = ResolvedConfig::default();
    second
        .model
        .extra_headers
        .insert("X-Import-Secret".to_string(), second_secret.to_string());
    second.approve = Some(crate::config::ApproveConfig::from_model(&second.model));
    let (second_generation, _) = controller
        .stage_initial_setup_config_import(second)
        .expect("stage replacement import");
    assert!(
        controller
            .pending_initial_setup_config_import
            .as_ref()
            .unwrap()
            .config
            .approve
            .is_some()
    );
    let redacted_values = ConfigField::ALL
        .into_iter()
        .filter(|field| !field.is_host_owned_generation())
        .map(|field| {
            (
                field.label().to_string(),
                field.public_value(controller.state.global_config()).value,
            )
        })
        .collect::<Vec<_>>();
    assert!(
        controller
            .hydrate_initial_setup_import_sensitive_values(
                redacted_values.clone(),
                Some(first_generation),
            )
            .is_err(),
        "a second import must invalidate the first raw owner"
    );
    let sparse_commit_values = redacted_values
        .iter()
        .filter(|(key, _)| {
            !ConfigField::ALL.into_iter().any(|field| {
                field.label() == key
                    && (field.is_connection_private_value()
                        || field.label().starts_with("approve."))
            })
        })
        .cloned()
        .collect();
    let sparse_hydrated = controller
        .hydrate_initial_setup_import_sensitive_values(
            sparse_commit_values,
            Some(second_generation),
        )
        .expect("current imported snapshot with unchanged private fields omitted");
    assert!(
        sparse_hydrated.iter().any(|(key, value)| {
            key == ConfigField::ExtraHeadersJson.label() && value.contains(second_secret)
        }),
        "same-target Finish must retain the imported header when the Web commit omits its redacted field"
    );
    for (field, value) in [
        (ConfigField::BaseUrl, "http://changed-import-target.test"),
        (ConfigField::ProviderProfile, "openai"),
    ] {
        for omit_private in [false, true] {
            let changed_target_values = redacted_values
                .iter()
                .filter(|(key, _)| {
                    !omit_private
                        || !ConfigField::ALL.into_iter().any(|field| {
                            field.label() == key && field.is_connection_private_value()
                        })
                })
                .map(|(key, text)| {
                    (
                        key.clone(),
                        if key == field.label() {
                            value.to_string()
                        } else {
                            text.clone()
                        },
                    )
                })
                .collect();
            let hydrated = controller
                .hydrate_initial_setup_import_sensitive_values(
                    changed_target_values,
                    Some(second_generation),
                )
                .expect("changed imported connection target");
            assert!(
                !hydrated.iter().any(|(key, text)| {
                    key == ConfigField::ExtraHeadersJson.label() && text.contains(second_secret)
                }),
                "an omitted or redacted header must not authorize the changed imported target"
            );
        }
    }
    let hydrated = controller
        .hydrate_initial_setup_import_sensitive_values(
            redacted_values.clone(),
            Some(second_generation),
        )
        .expect("current import generation");
    let headers = hydrated
        .iter()
        .find(|(key, _)| key == ConfigField::ExtraHeadersJson.label())
        .map(|(_, value)| value.as_str())
        .expect("hydrated headers");
    assert!(headers.contains(second_secret));
    assert!(!headers.contains(first_secret));

    let explicit = redacted_values
        .into_iter()
        .map(|(key, value)| {
            if key == ConfigField::ExtraHeadersJson.label() {
                (key, "{\"X-Manual\":\"manual-value\"}".to_string())
            } else {
                (key, value)
            }
        })
        .collect();
    let explicit = controller
        .hydrate_initial_setup_import_sensitive_values(explicit, Some(second_generation))
        .expect("explicit sensitive replacement");
    let headers = explicit
        .iter()
        .find(|(key, _)| key == ConfigField::ExtraHeadersJson.label())
        .map(|(_, value)| value.as_str())
        .expect("explicit headers");
    assert!(headers.contains("manual-value"));
    assert!(!headers.contains(second_secret));

    controller.state.provider_config.config_generation = controller
        .state
        .provider_config
        .config_generation
        .saturating_add(1);
    controller.reconcile_pending_initial_setup_config_import_owner();
    assert!(
        controller.pending_initial_setup_config_import.is_none(),
        "raw imported secrets must be dropped as soon as their exact config owner drifts"
    );
    assert!(
        controller
            .hydrate_initial_setup_import_sensitive_values(Vec::new(), Some(second_generation),)
            .is_err(),
        "another settings owner cannot reuse the imported approval snapshot"
    );
}

#[test]
fn only_typed_interruptions_suppress_desktop_failure_notifications() {
    let interruption = RunCancellationCause::Interruption(TurnInterruptionCause::UserStop);
    let failure = RunCancellationCause::Failure("provider unavailable".to_string());
    let superseded = RunCancellationCause::Superseded;

    assert!(desktop_run_failure_notification_allowed(None));
    assert!(!desktop_run_failure_notification_allowed(Some(
        &interruption,
    )));
    assert!(desktop_run_failure_notification_allowed(Some(&failure)));
    assert!(desktop_run_failure_notification_allowed(Some(&superseded)));
}

#[test]
fn current_session_access_change_persists_global_then_session() {
    let session_id = SessionId::new();
    let remembered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let persisted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let result = persist_desktop_access_mode_owners(
        crate::config::AccessMode::Default,
        crate::config::AccessMode::FullAccess,
        Some(session_id),
        {
            let remembered = remembered.clone();
            move |expected, mode| {
                remembered
                    .lock()
                    .expect("remembered")
                    .push((expected, mode));
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            }
        },
        {
            let persisted = persisted.clone();
            move |owner, mode| {
                persisted.lock().expect("persisted").push((owner, mode));
                Ok(())
            }
        },
    );

    assert_eq!(result, Ok(Utf8PathBuf::from("C:/config.toml")));
    assert_eq!(
        *remembered.lock().expect("remembered"),
        vec![(
            crate::config::AccessMode::Default,
            crate::config::AccessMode::FullAccess
        )]
    );
    assert_eq!(
        *persisted.lock().expect("persisted"),
        vec![(session_id, crate::config::AccessMode::FullAccess)]
    );
}

#[test]
fn no_session_access_change_persists_only_global_owner() {
    let session_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result = persist_desktop_access_mode_owners(
        crate::config::AccessMode::Default,
        crate::config::AccessMode::FullAccess,
        None,
        |expected, mode| {
            assert_eq!(expected, crate::config::AccessMode::Default);
            assert_eq!(mode, crate::config::AccessMode::FullAccess);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        {
            let session_calls = session_calls.clone();
            move |_, _| {
                session_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
    );

    assert_eq!(result, Ok(Utf8PathBuf::from("C:/config.toml")));
    assert_eq!(session_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn access_persistence_completion_accepts_only_the_same_owner_and_runtime_epoch() {
    let session_id = SessionId::new();
    let target = AccessModePersistenceTarget {
        operation_id: DesktopAsyncOperationId::from_test_value(1),
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        session_id: Some(session_id),
        config_generation: 7,
        root_run_generation: Some(4),
        runtime_owner_token: "root:4".to_string(),
        old_global_access_mode: crate::config::AccessMode::Default,
        old_effective_access_mode: crate::config::AccessMode::Default,
        access_mode: crate::config::AccessMode::FullAccess,
    };

    assert!(access_mode_persistence_target_matches(
        &target,
        Utf8Path::new("C:/workspace"),
        Some(session_id),
        7,
        "root:4",
    ));
    for current in ["tree:4", "idle:4"] {
        assert!(
            access_mode_persistence_target_matches(
                &target,
                Utf8Path::new("C:/workspace"),
                Some(session_id),
                7,
                current,
            ),
            "root-owned access persistence may settle after the same epoch crosses to {current}"
        );
    }
    assert!(!access_mode_persistence_target_matches(
        &target,
        Utf8Path::new("C:/other"),
        Some(session_id),
        7,
        "root:4",
    ));
    assert!(!access_mode_persistence_target_matches(
        &target,
        Utf8Path::new("C:/workspace"),
        Some(SessionId::new()),
        7,
        "root:4",
    ));
    assert!(!access_mode_persistence_target_matches(
        &target,
        Utf8Path::new("C:/workspace"),
        Some(session_id),
        8,
        "root:4",
    ));
    assert!(!access_mode_persistence_target_matches(
        &target,
        Utf8Path::new("C:/workspace"),
        Some(session_id),
        7,
        "root:5",
    ));

    let tree_target = AccessModePersistenceTarget {
        runtime_owner_token: "tree:4".to_string(),
        ..target.clone()
    };
    for current in ["tree:4", "root:4", "idle:4"] {
        assert!(
            access_mode_persistence_target_matches(
                &tree_target,
                Utf8Path::new("C:/workspace"),
                Some(session_id),
                7,
                current,
            ),
            "tree-owned access persistence may settle exactly or after the same child tree returns to {current}"
        );
    }
    for current in ["root:5", "tree:5", "idle:5"] {
        assert!(
            !access_mode_persistence_target_matches(
                &tree_target,
                Utf8Path::new("C:/workspace"),
                Some(session_id),
                7,
                current,
            ),
            "a different runtime epoch must be rejected: {current}"
        );
    }

    let idle_target = AccessModePersistenceTarget {
        runtime_owner_token: "idle:4".to_string(),
        ..target.clone()
    };
    assert!(access_mode_persistence_target_matches(
        &idle_target,
        Utf8Path::new("C:/workspace"),
        Some(session_id),
        7,
        "idle:4",
    ));
    for current in ["root:4", "tree:4"] {
        assert!(
            !access_mode_persistence_target_matches(
                &idle_target,
                Utf8Path::new("C:/workspace"),
                Some(session_id),
                7,
                current,
            ),
            "an idle capture cannot adopt a later active phase: {current}"
        );
    }

    let pre_admission_target = AccessModePersistenceTarget {
        session_id: None,
        ..target
    };
    assert_eq!(
        access_mode_persistence_target_relation(
            &pre_admission_target,
            Utf8Path::new("C:/workspace"),
            Some(session_id),
            7,
            "root:4",
        ),
        AccessModePersistenceTargetRelation::AdoptedSession(session_id)
    );
    assert_eq!(
        access_mode_persistence_target_relation(
            &pre_admission_target,
            Utf8Path::new("C:/workspace"),
            Some(session_id),
            7,
            "tree:4",
        ),
        AccessModePersistenceTargetRelation::AdoptedSession(session_id)
    );
    assert_eq!(
        access_mode_persistence_target_relation(
            &pre_admission_target,
            Utf8Path::new("C:/workspace"),
            Some(session_id),
            8,
            "root:4",
        ),
        AccessModePersistenceTargetRelation::Stale
    );
}

#[test]
fn global_access_failure_does_not_touch_the_current_session() {
    let session_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result = persist_desktop_access_mode_owners(
        crate::config::AccessMode::Default,
        crate::config::AccessMode::FullAccess,
        Some(SessionId::new()),
        |_, _| Err("global failed".to_string()),
        {
            let session_calls = session_calls.clone();
            move |_, _| {
                session_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
    );

    assert_eq!(
        result,
        Err("global access mode update failed: global failed".to_string())
    );
    assert_eq!(session_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn session_access_failure_compensates_the_global_field() {
    let remembered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let result = persist_desktop_access_mode_owners(
        crate::config::AccessMode::Default,
        crate::config::AccessMode::FullAccess,
        Some(SessionId::new()),
        {
            let remembered = remembered.clone();
            move |expected, mode| {
                remembered
                    .lock()
                    .expect("remembered")
                    .push((expected, mode));
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            }
        },
        |_, _| Err("session failed".to_string()),
    );

    assert_eq!(
        result,
        Err(
            "session access mode update failed and the global field was restored: session failed"
                .to_string()
        )
    );
    assert_eq!(
        *remembered.lock().expect("remembered"),
        vec![
            (
                crate::config::AccessMode::Default,
                crate::config::AccessMode::FullAccess
            ),
            (
                crate::config::AccessMode::FullAccess,
                crate::config::AccessMode::Default
            )
        ]
    );
}

#[test]
fn adopted_session_access_failure_uses_the_same_cas_compensation() {
    let remembered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let worker = AccessModePersistenceWorker::new(
        {
            let remembered = remembered.clone();
            move |expected, mode| {
                remembered
                    .lock()
                    .expect("remembered")
                    .push((expected, mode));
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            }
        },
        |_, _| Err("adopted session failed".to_string()),
    );
    let target = AccessModePersistenceTarget {
        operation_id: DesktopAsyncOperationId::from_test_value(1),
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        session_id: None,
        config_generation: 1,
        root_run_generation: Some(1),
        runtime_owner_token: "root:1".to_string(),
        old_global_access_mode: crate::config::AccessMode::Default,
        old_effective_access_mode: crate::config::AccessMode::Default,
        access_mode: crate::config::AccessMode::FullAccess,
    };

    let commit = worker
        .persist_initial_owners(&target)
        .expect("global-only first phase");
    let error = worker
        .persist_adopted_session(&target, SessionId::new(), commit.remembered_path)
        .expect_err("adopted session failure");

    assert!(error.contains("global field was restored"));
    assert_eq!(
        *remembered.lock().expect("remembered"),
        vec![
            (
                crate::config::AccessMode::Default,
                crate::config::AccessMode::FullAccess
            ),
            (
                crate::config::AccessMode::FullAccess,
                crate::config::AccessMode::Default
            )
        ]
    );
}

#[tokio::test]
async fn desktop_current_session_access_is_durable_for_tui_reopen_and_rejects_child_owner() {
    use crate::session::{NewSession, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let initial_access_mode = crate::config::AccessMode::Default;
    let args = DesktopArgs {
        directory: Some(root.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let create_session = |title: &str| NewSession {
        project_id: controller.app.workspace.project_id,
        title: title.to_string(),
        cwd: root.clone(),
        model: controller.app.config.model.model.clone(),
        base_url: controller.app.config.model.base_url.clone(),
        access_mode: initial_access_mode,
        provider_connection: None,
    };
    let repository = controller.app.store.session_repo();
    let root_session = repository
        .create_session(create_session("root"))
        .await
        .expect("root session");
    let child_session = repository
        .create_session(create_session("child"))
        .await
        .expect("child session");
    repository
        .insert_session_spawn_edge(
            root_session.id,
            root_session.id,
            child_session.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    controller.app.config.permissions.access_mode = initial_access_mode;
    controller
        .state
        .provider_config
        .update_access_mode(initial_access_mode);
    controller.state.app_state.current_session_id = Some(root_session.id);
    controller.run_lifecycle.begin(1, RunControl::new());
    let expected_access_mode = initial_access_mode.next();
    let session_service = controller.app.session_service.clone();
    let persisted_service = session_service.clone();

    assert!(controller.start_access_mode_persistence(
        move |expected, access_mode| {
            assert_eq!(expected, initial_access_mode);
            assert_eq!(access_mode, expected_access_mode);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        move |session_id, access_mode| {
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    persisted_service
                        .update_root_session_access_mode(session_id, access_mode)
                        .await
                        .map(|update| Some(update.session))
                        .map_err(|error| error.to_string())
                })
            })
            .join()
            .map_err(|_| "session worker panicked".to_string())?
        },
    ));
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!controller.state.background_mutation_pending());

    let reopened = session_service
        .get_session(root_session.id)
        .await
        .expect("TUI durable reopen source");
    assert_eq!(reopened.access_mode, expected_access_mode);
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        reopened.access_mode
    );
    assert!(
        session_service
            .update_root_session_access_mode(child_session.id, expected_access_mode)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn desktop_child_only_tree_can_change_root_access_and_settle_after_tree_finishes() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let root_session_id = SessionId::new();
    let child_session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(root_session_id);
    controller.loaded_agent_activity_records = Some((
        root_session_id,
        vec![agent_record(
            child_session_id,
            "/root/active_child",
            AgentStatus::Running,
            "",
        )],
    ));
    assert!(controller.current_agent_tree_active());
    assert!(!controller.run_lifecycle.root_is_active());
    assert!(controller.access_mode_mutation_admission_open());
    assert_eq!(
        controller.access_mode_mutation_runtime_contract().0,
        "tree:0"
    );

    let mut persisted = None;
    assert!(controller.toggle_access_mode_with_persistence(
        |expected, access_mode| {
            assert_eq!(expected, crate::config::AccessMode::Default);
            assert_eq!(access_mode, crate::config::AccessMode::AutoReview);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        |session_id, access_mode| {
            persisted = Some((session_id, access_mode));
            Ok(())
        },
    ));
    assert_eq!(
        persisted,
        Some((root_session_id, crate::config::AccessMode::AutoReview))
    );

    let target = AccessModePersistenceTarget {
        operation_id: controller.state.begin_access_mode_persistence(),
        workspace_root: controller.app.workspace.root.clone(),
        session_id: Some(root_session_id),
        config_generation: controller.state.provider_config.config_generation,
        root_run_generation: None,
        runtime_owner_token: "tree:0".to_string(),
        old_global_access_mode: crate::config::AccessMode::Default,
        old_effective_access_mode: crate::config::AccessMode::Default,
        access_mode: crate::config::AccessMode::AutoReview,
    };
    controller.loaded_agent_activity_records = Some((root_session_id, Vec::new()));
    assert!(!controller.current_agent_tree_active());
    assert_eq!(
        controller.access_mode_mutation_runtime_contract().0,
        "idle:0"
    );
    assert_eq!(
        controller.access_mode_persistence_target_relation(&target),
        AccessModePersistenceTargetRelation::Exact,
        "tree:N to idle:N keeps the same root-session access owner"
    );

    controller.run_lifecycle.begin(1, RunControl::new());
    controller.next_root_run_generation = 2;
    assert_eq!(
        controller.access_mode_persistence_target_relation(&target),
        AccessModePersistenceTargetRelation::Stale,
        "a new root generation revokes the finished tree completion grace"
    );
}

#[tokio::test]
async fn desktop_child_only_tree_does_not_block_new_root_prompt_admission() {
    use crate::session::{NewSession, SessionRepository as _};

    let (_temp, workspace_root, mut controller) = empty_access_test_controller().await;
    let root_session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "independent root terminal".to_string(),
            cwd: workspace_root,
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("root session");
    controller.state.app_state.current_session_id = Some(root_session.id);
    controller.state.app_state.current_session_title = root_session.title;
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Completed;
    controller.loaded_agent_activity_records = Some((
        root_session.id,
        vec![agent_record(
            SessionId::new(),
            "/root/detached_child",
            AgentStatus::Running,
            "",
        )],
    ));

    assert!(controller.current_agent_tree_active());
    assert!(!controller.run_lifecycle.root_is_active());
    let admitted_generation = controller.next_root_run_generation;

    assert!(
        controller.start_run("continue with a new root turn".to_string()),
        "a descendant worker is not the root prompt admission owner"
    );
    assert_eq!(
        controller.run_lifecycle.root_generation(),
        Some(admitted_generation)
    );
    assert_eq!(
        controller
            .pending_root_submission
            .as_ref()
            .map(|pending| pending.run_generation),
        Some(admitted_generation)
    );
    assert_eq!(controller.next_root_run_generation, admitted_generation + 1);

    // This test owns no provider response. Dropping the exact worker owner aborts the
    // pre-admitted task before the controller's local executor is shut down.
    drop(controller.run_lifecycle.root.take());
}

#[tokio::test]
async fn pre_admission_access_change_persists_the_same_root_session_adopted_before_completion() {
    use crate::session::{NewSession, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let initial_access_mode = crate::config::AccessMode::Default;
    let args = DesktopArgs {
        directory: Some(root.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "adopted root".to_string(),
            cwd: root,
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: initial_access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    controller
        .app
        .store
        .session_repo()
        .admit_session_turn(session.id, crate::protocol::TurnId::new())
        .await
        .expect("active root admission")
        .expect("active root admitted");
    controller.app.config.permissions.access_mode = initial_access_mode;
    controller
        .state
        .provider_config
        .update_access_mode(initial_access_mode);
    controller.run_lifecycle.begin(1, RunControl::new());
    let expected_access_mode = initial_access_mode.next();
    let persisted_service = controller.app.session_service.clone();
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);

    assert!(controller.start_access_mode_persistence(
        move |expected, access_mode| {
            assert_eq!(expected, initial_access_mode);
            assert_eq!(access_mode, expected_access_mode);
            started_tx.send(()).expect("signal global worker");
            release_rx.recv().expect("release global worker");
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        move |session_id, access_mode| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async move {
                persisted_service
                    .update_root_session_access_mode(session_id, access_mode)
                    .await
                    .map(|update| Some(update.session))
                    .map_err(|error| error.to_string())
            })
        },
    ));
    started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("global worker started");
    controller
        .runtime_tx
        .send(RuntimeMessage::RunEvent {
            run_generation: 1,
            event: RunEvent::SessionStarted {
                session_id: session.id,
                title: session.title.clone(),
            },
        })
        .expect("session adoption event");
    controller.drain_runtime_messages();
    assert_eq!(
        controller.state.app_state.current_session_id,
        Some(session.id)
    );
    release_tx.send(()).expect("release global worker");
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!controller.state.background_mutation_pending());
    assert_eq!(
        controller
            .app
            .session_service
            .get_session(session.id)
            .await
            .expect("durable adopted root")
            .access_mode,
        expected_access_mode
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        expected_access_mode
    );

    controller.next_root_run_generation = 2;
    controller.run_lifecycle.finish_root();
    let finished_root_target = AccessModePersistenceTarget {
        operation_id: DesktopAsyncOperationId::from_test_value(99),
        workspace_root: controller.app.workspace.root.clone(),
        session_id: None,
        config_generation: controller.state.provider_config.config_generation,
        root_run_generation: Some(1),
        runtime_owner_token: "root:1".to_string(),
        old_global_access_mode: initial_access_mode,
        old_effective_access_mode: initial_access_mode,
        access_mode: expected_access_mode,
    };
    assert_eq!(
        controller.access_mode_persistence_target_relation(&finished_root_target),
        AccessModePersistenceTargetRelation::AdoptedSession(session.id),
        "completion from the just-finished generation retains its exact admitted owner"
    );
    controller.next_root_run_generation = 3;
    assert_eq!(
        controller.access_mode_persistence_target_relation(&finished_root_target),
        AccessModePersistenceTargetRelation::Stale,
        "a newer root generation revokes the terminal completion grace"
    );
}

#[tokio::test]
async fn pre_admission_access_change_waits_for_session_started_after_global_completion() {
    use crate::session::{NewSession, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let initial_access_mode = crate::config::AccessMode::Default;
    let args = DesktopArgs {
        directory: Some(root.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "late adopted root".to_string(),
            cwd: root,
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: initial_access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    controller
        .app
        .store
        .session_repo()
        .admit_session_turn(session.id, crate::protocol::TurnId::new())
        .await
        .expect("late active root admission")
        .expect("late active root admitted");
    controller.app.config.permissions.access_mode = initial_access_mode;
    controller
        .state
        .provider_config
        .update_access_mode(initial_access_mode);
    controller.run_lifecycle.begin(1, RunControl::new());
    let expected_access_mode = initial_access_mode.next();
    let persisted_service = controller.app.session_service.clone();

    assert!(controller.start_access_mode_persistence(
        move |expected, access_mode| {
            assert_eq!(expected, initial_access_mode);
            assert_eq!(access_mode, expected_access_mode);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        move |session_id, access_mode| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async move {
                persisted_service
                    .update_root_session_access_mode(session_id, access_mode)
                    .await
                    .map(|update| Some(update.session))
                    .map_err(|error| error.to_string())
            })
        },
    ));
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if controller.pending_access_mode_adoption.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(controller.pending_access_mode_adoption.is_some());
    assert!(controller.state.background_mutation_pending());
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        initial_access_mode,
        "the next-turn owner is not committed until both durable owners succeed"
    );

    controller
        .runtime_tx
        .send(RuntimeMessage::RunEvent {
            run_generation: 1,
            event: RunEvent::SessionStarted {
                session_id: session.id,
                title: session.title.clone(),
            },
        })
        .expect("late session adoption event");
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!controller.state.background_mutation_pending());
    assert!(controller.pending_access_mode_adoption.is_none());
    assert_eq!(
        controller
            .app
            .session_service
            .get_session(session.id)
            .await
            .expect("durable late adopted root")
            .access_mode,
        expected_access_mode
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        expected_access_mode
    );
}

async fn empty_access_test_controller() -> (tempfile::TempDir, Utf8PathBuf, DesktopController) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let args = DesktopArgs {
        directory: Some(root.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    (temp, root, controller)
}

#[tokio::test]
async fn desktop_goal_query_completes_without_a_turn_or_failed_history() {
    use crate::session::{NewSession, SessionRepository as _};
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "Goal control".to_string(),
            cwd: root.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    let before = load_latest_session_detail(&controller.app, session.id)
        .await
        .expect("before");
    controller.state.load_open_session(&before.read);
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Completed;
    let generation = controller.composer_commit_generation;
    let retained_image = root.join("next-turn.png");
    controller
        .state
        .composer
        .image_attachment_paths
        .push(retained_image.clone());
    assert!(controller.start_run("/goal".to_string()));
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if !controller.run_lifecycle.root_is_active() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(!controller.run_lifecycle.root_is_active());
    assert!(controller.pending_root_submission.is_none());
    assert_eq!(controller.state.status_code, DesktopStatusCode::GoalControl);
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Completed
    );
    assert_eq!(controller.composer_commit_generation, generation + 1);
    assert!(!controller.state.post_run_refresh_pending());
    assert!(
        controller.state.app_state.transcript_entries.is_empty(),
        "control completion must not fabricate a User or failure entry"
    );
    assert_eq!(
        controller.state.composer.image_attachment_paths,
        vec![retained_image]
    );
    assert!(
        controller
            .state
            .app_state
            .status_message
            .as_deref()
            .unwrap()
            .contains("Goal が設定されていません")
    );
    let after = load_latest_session_detail(&controller.app, session.id)
        .await
        .expect("after");
    assert_eq!(
        serde_json::to_value(&after.read).unwrap(),
        serde_json::to_value(&before.read).unwrap(),
        "Goal query must not create or rewrite a durable Turn"
    );
}

#[tokio::test]
async fn desktop_goal_stale_completion_cannot_consume_a_newer_pending_input() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    controller.run_lifecycle.begin(42, RunControl::new());
    controller.state.begin_agent_run();
    controller.pending_root_submission = Some(PendingRootSubmission {
        run_generation: 42,
        owner_workspace_path: controller.root_submission_owner_workspace_path(),
        owner_session_id: None,
        prompt_dispatch: crate::session::PromptDispatchPart::raw("newer request"),
        image_paths: Vec::new(),
        prompt_review_to_cancel: None,
    });
    let generation = controller.composer_commit_generation;
    controller.settle_control_finished(
        41,
        DesktopControlResult::GoalGet(ThreadGoalGetResult { goal: None }),
    );
    assert!(controller.run_lifecycle.owns(42));
    assert_eq!(
        controller
            .pending_root_submission
            .as_ref()
            .unwrap()
            .run_generation,
        42
    );
    assert_eq!(controller.composer_commit_generation, generation);
    assert_ne!(controller.state.status_code, DesktopStatusCode::GoalControl);
}

#[test]
fn desktop_goal_renderer_returns_query_and_mutation_results_without_run_events() {
    let (runtime_tx, runtime_rx) = mpsc::sync_channel(1);
    let (tx, rx) = mpsc::channel();
    let mut renderer = DesktopRenderer {
        runtime_tx,
        bootstrap_control: DesktopControlPlaneSender { tx },
        run_generation: 1,
        notification_title: "Goal".to_string(),
        notified_terminal: false,
        control_result: None,
    };
    renderer
        .render_thread_goal_get(&ThreadGoalGetResult { goal: None })
        .unwrap();
    assert!(
        renderer
            .control_result
            .as_ref()
            .unwrap()
            .description()
            .contains("設定されていません")
    );
    let goal = crate::session::ThreadGoal {
        thread_id: SessionId::new(),
        objective: "first line\nsecond line".to_string(),
        status: crate::session::ThreadGoalStatus::Paused,
        token_budget: Some(100),
        tokens_used: 7,
        time_used_seconds: 3,
        created_at: 1,
        updated_at: 2,
    };
    renderer
        .render_thread_goal_set(&ThreadGoalSetResult { goal: goal.clone() })
        .unwrap();
    let description = renderer.control_result.as_ref().unwrap().description();
    assert!(description.contains("一時停止\nfirst line\nsecond line"));
    assert!(description.contains("7 / 100"));
    renderer
        .render_thread_goal_clear(&ThreadGoalClearResult {
            thread_id: goal.thread_id,
            cleared: true,
        })
        .unwrap();
    assert!(
        renderer
            .control_result
            .as_ref()
            .unwrap()
            .description()
            .contains("削除しました")
    );
    assert!(runtime_rx.try_recv().is_err());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn session_settings_cas_loser_returns_canonical_record_for_conflict_rebase() {
    use crate::session::{NewSession, SessionRepository as _};

    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let initial = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "root".to_string(),
            cwd: root,
            model: "initial-model".to_string(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("root session");
    let loaded = load_latest_session_detail(&controller.app, initial.id)
        .await
        .expect("canonical root detail");
    controller.state.load_open_session(&loaded.read);

    let persistence = controller
        .prepare_root_session_settings_persistence(
            initial.session_settings_revision,
            SessionSettingsPatch {
                model: Some("losing-model".to_string()),
                ..SessionSettingsPatch::default()
            },
        )
        .expect("persistence request")
        .expect("matching in-memory revision");
    let winner = controller
        .app
        .session_service
        .compare_and_set_root_session_settings(
            initial.id,
            initial.session_settings_revision,
            SessionSettingsPatch {
                model: Some("winning-model".to_string()),
                ..SessionSettingsPatch::default()
            },
        )
        .await
        .expect("winning settings write")
        .expect("winning CAS update");
    let access_only = persistence.access_only();

    let outcome = tokio::task::spawn_blocking(move || persistence.execute_blocking())
        .await
        .expect("persistence worker")
        .expect("canonical conflict outcome");
    let RootSessionSettingsPersistenceOutcome::Conflict { session: canonical } = outcome else {
        panic!("the stale writer must return the canonical conflict owner");
    };
    assert!(!access_only);
    assert_eq!(canonical.id, initial.id);
    assert_eq!(canonical.model, "winning-model");
    assert_eq!(
        canonical.session_settings_revision,
        winner.session.session_settings_revision
    );

    assert!(
        controller
            .state
            .apply_persisted_root_session_record(canonical.clone())
    );
    let open = controller
        .state
        .open_session
        .as_ref()
        .expect("rebased open session")
        .session();
    assert_eq!(open.model, "winning-model");
    assert_eq!(
        open.session_settings_revision,
        winner.session.session_settings_revision
    );

    assert!(
        !controller
            .state
            .apply_persisted_root_session_record(initial),
        "a superseded persistence completion must not downgrade a newer canonical revision"
    );
    assert_eq!(
        controller
            .state
            .open_session
            .as_ref()
            .expect("newer open session remains")
            .session()
            .session_settings_revision,
        winner.session.session_settings_revision
    );
}

#[tokio::test]
async fn initial_setup_session_apply_keeps_the_finish_only_startup_latch() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let persisted_model = controller.app.config.model.model.clone();
    controller.state.begin_startup(false, None, &root);
    assert!(controller.state.startup.requires_initial_setup());
    assert!(controller.state.view.startup_overlay_forced);
    assert_eq!(
        controller.state.view.overlay,
        super::super::state::DesktopOverlay::InitialSetup
    );

    let temporary_model = "temporary-initial-setup-model";
    let values = ConfigEditorState::from_config(&controller.state.provider_config.effective_config)
        .fields
        .into_iter()
        .map(|field| {
            let value = if field.key == ConfigField::Model {
                temporary_model.to_string()
            } else {
                field.value
            };
            (field.key.label().to_string(), value)
        })
        .collect();

    assert!(controller.apply_session_config(values));
    assert!(controller.state.startup.requires_initial_setup());
    assert!(controller.state.view.startup_overlay_forced);
    assert_eq!(
        controller.state.view.overlay,
        super::super::state::DesktopOverlay::InitialSetup
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .model
            .model,
        temporary_model
    );
    assert_eq!(
        controller.app.config.model.model, persisted_model,
        "session Apply must not replace the persisted global config owner"
    );
}

async fn side_chat_test_controller()
-> (tempfile::TempDir, Utf8PathBuf, DesktopController, SessionId) {
    use crate::session::SessionRepository as _;

    let (temp, root, mut controller) = empty_access_test_controller().await;
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(crate::session::NewSession {
            project_id: controller.app.workspace.project_id,
            title: "side chat owner".to_string(),
            cwd: root.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("side chat owner session");
    let snapshot = load_snapshot_for_selection(&controller.app, Some(session.id))
        .await
        .expect("owner snapshot");
    controller.state.replace_snapshot(snapshot);
    controller.state.app_state.current_session_id = Some(session.id);
    controller.state.app_state.current_session_title = session.title;
    (temp, root, controller, session.id)
}

#[tokio::test]
async fn side_chat_configuration_owns_its_trimmed_system_prompt_without_main_inheritance() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .state
        .provider_config
        .effective_config
        .model
        .system_prompt = "MAIN_PRIVATE_PROMPT".to_string();

    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:1234".to_string(),
            "side-model".to_string(),
            "  SIDE_PRIVATE_PROMPT  ".to_string(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat with its own prompt");
    let binding = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    assert_eq!(binding.system_prompt, "SIDE_PRIVATE_PROMPT");
    let projection = controller.side_chat_projection();
    assert_eq!(projection.system_prompt, "SIDE_PRIVATE_PROMPT");
    let debug = format!("{projection:?}");
    assert!(debug.contains("system_prompt_chars"));
    assert!(!debug.contains("SIDE_PRIVATE_PROMPT"));
    assert!(!debug.contains("MAIN_PRIVATE_PROMPT"));

    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:1234".to_string(),
            "side-model".to_string(),
            "   ".to_string(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("clear side chat prompt");
    let binding = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    assert!(binding.system_prompt.is_empty());
}

#[tokio::test]
async fn global_side_chat_defaults_are_snapshotted_until_the_conversation_is_closed() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    {
        let defaults = &mut controller.state.provider_config.effective_config.side_chat;
        defaults.base_url = "http://127.0.0.1:4111".to_string();
        defaults.model = "first-side-model".to_string();
        defaults.system_prompt = "FIRST_SIDE_PROMPT".to_string();
        defaults.provider_profile = ProviderProfile::OpenAiCompatible;
        defaults.api_key_env = Some("FIRST_SIDE_KEY".to_string());
        defaults.context_window = 65_536;
    }

    controller
        .ensure_side_chat(owner_session_id)
        .expect("materialize global Side Chat defaults");
    let first = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("read first binding")
        .expect("first binding");
    assert_eq!(first.model, "first-side-model");
    assert_eq!(first.system_prompt, "FIRST_SIDE_PROMPT");
    assert_eq!(first.context_window, 65_536);
    assert_eq!(first.api_key_env.as_deref(), Some("FIRST_SIDE_KEY"));

    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .model = "second-side-model".to_string();
    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .system_prompt = "SECOND_SIDE_PROMPT".to_string();
    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .api_key_env = Some("SECOND_SIDE_KEY".to_string());
    controller
        .ensure_side_chat(owner_session_id)
        .expect("existing Side Chat remains stable");
    let unchanged = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("read stable binding")
        .expect("stable binding");
    assert_eq!(unchanged.id, first.id);
    assert_eq!(unchanged.model, "first-side-model");
    assert_eq!(unchanged.system_prompt, "FIRST_SIDE_PROMPT");
    assert_eq!(unchanged.api_key_env, first.api_key_env);
    assert_eq!(
        controller.side_chat_projection().api_key_env,
        "FIRST_SIDE_KEY"
    );
    let request = side_chat_request_profile(&unchanged);
    assert_eq!(request.api_key_env, first.api_key_env);
    assert_eq!(request.base_url, first.base_url);
    assert!(request.extra_headers.is_empty());

    controller
        .delete_side_chat(owner_session_id, first.id, first.request_generation)
        .expect("close first Side Chat");
    assert!(
        controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("read deleted binding")
            .is_none()
    );
    controller
        .ensure_side_chat(owner_session_id)
        .expect("materialize updated global defaults");
    let replacement = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("read replacement binding")
        .expect("replacement binding");
    assert_ne!(replacement.id, first.id);
    assert_eq!(replacement.model, "second-side-model");
    assert_eq!(replacement.system_prompt, "SECOND_SIDE_PROMPT");
    assert_eq!(replacement.api_key_env.as_deref(), Some("SECOND_SIDE_KEY"));
    assert_eq!(
        controller.side_chat_projection().api_key_env,
        "SECOND_SIDE_KEY"
    );
}

#[tokio::test]
async fn side_chat_authentication_requires_an_explicit_independent_reference() {
    let (_temp, _root, mut controller, owner) = side_chat_test_controller().await;
    controller
        .state
        .provider_config
        .effective_config
        .model
        .api_key_env = Some("SHARED_PROVIDER_KEY".to_string());
    controller
        .state
        .provider_config
        .effective_config
        .model
        .extra_headers
        .insert("X-Main-Secret".to_string(), "main-only-header".to_string());
    controller
        .ensure_side_chat(owner)
        .expect("anonymous Side default");
    let binding = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner)
        .unwrap()
        .unwrap();
    assert_eq!(binding.api_key_env, None);
    assert_eq!(side_chat_request_profile(&binding).api_key_env, None);
    assert!(side_chat_request_profile(&binding).extra_headers.is_empty());
    controller
        .delete_side_chat(owner, binding.id, binding.request_generation)
        .unwrap();

    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .api_key_env = Some("SHARED_PROVIDER_KEY".to_string());
    controller
        .ensure_side_chat(owner)
        .expect("explicit same reference");
    let binding = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner)
        .unwrap()
        .unwrap();
    assert_eq!(
        side_chat_request_profile(&binding).api_key_env.as_deref(),
        Some("SHARED_PROVIDER_KEY")
    );
    assert!(side_chat_request_profile(&binding).extra_headers.is_empty());
    assert_eq!(
        controller.side_chat_projection().api_key_env,
        "SHARED_PROVIDER_KEY"
    );
}

#[tokio::test]
async fn hub_origin_side_chat_requires_explicit_direct_capture_before_admission() {
    let (_temp, _root, mut controller, owner) = side_chat_test_controller().await;
    let mut hub_target = SideChatProviderTarget::try_from(
        &controller.state.provider_config.effective_config.side_chat,
    )
    .expect("Side policy");
    hub_target.route_kind = SideChatRouteKind::Hub;
    hub_target.model = "logical-hub-model".into();
    hub_target.base_url = "https://hub.test:9471".into();
    let binding = controller
        .app
        .store
        .side_chat_repo()
        .ensure(owner, hub_target)
        .unwrap();
    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .base_url
        .clear();
    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .model
        .clear();
    controller
        .ensure_side_chat(owner)
        .expect("reopen needs no unrelated Direct settings");
    let projection = controller.side_chat_projection();
    assert!(!projection.can_send);
    assert!(!projection.direct_provider_capture.unwrap().enabled);
    let error = controller
        .start_side_chat(
            owner,
            binding.id,
            0,
            0,
            None,
            None,
            "must not call the management URL".into(),
        )
        .expect_err("Direct capture required");
    assert!(error.contains("Direct設定"), "{error}");
    assert_eq!(
        controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner)
            .unwrap(),
        Some(binding.clone())
    );
    assert!(
        controller
            .app
            .store
            .side_chat_repo()
            .conversation_projection(owner)
            .unwrap()
            .unwrap()
            .messages
            .is_empty()
    );
    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .base_url = "http://direct.test:1234/v1".into();
    controller
        .state
        .provider_config
        .effective_config
        .side_chat
        .model = "direct-model".into();
    assert!(
        controller
            .side_chat_projection()
            .direct_provider_capture
            .unwrap()
            .enabled
    );
    controller
        .capture_side_chat_direct_provider(owner, binding.id, 0, 0)
        .expect("first Direct capture");
    let applied = controller.side_chat_projection();
    assert!(applied.direct_provider_capture.is_none());
    assert!(applied.can_send);
    assert_eq!(applied.model, "direct-model");
}

#[tokio::test]
async fn side_chat_draft_quote_is_atomic_replaces_without_orphan_and_rehydrates_after_restart() {
    let (_temp, root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:1234".to_string(),
            "side-model".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let initial = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("initial binding read")
        .expect("initial binding");
    let first_quote = SideChatQuoteRequest {
        source_kind: super::super::side_chat::SideChatQuoteSourceKind::Transcript,
        source_history_item_id: crate::protocol::HistoryItemId::new(),
        source_append_position: Some(41),
        selected_text: "first authority".to_string(),
    };
    controller
        .save_side_chat_draft(
            owner_session_id,
            initial.id,
            initial.draft_revision,
            "> Side Chat 引用\n> first authority\n\n".to_string(),
            Some(first_quote),
        )
        .expect("persist first typed quote");
    let after_first = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("first binding read")
        .expect("first binding");
    let second_quote = SideChatQuoteRequest {
        source_kind: super::super::side_chat::SideChatQuoteSourceKind::Artifact,
        source_history_item_id: crate::protocol::HistoryItemId::new(),
        source_append_position: Some(42),
        selected_text: "second authority".to_string(),
    };
    let second_text = "> Side Chat 引用\n> second authority\n\nExplain this.".to_string();
    controller
        .save_side_chat_draft(
            owner_session_id,
            after_first.id,
            after_first.draft_revision,
            second_text.clone(),
            Some(second_quote.clone()),
        )
        .expect("replace typed quote atomically");
    let after_second = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("second binding read")
        .expect("second binding");
    let decoded = decode_persisted_side_chat_draft(&after_second.persisted_draft)
        .expect("decode durable envelope");
    assert_eq!(decoded.text, second_text);
    assert_eq!(decoded.quote, Some(second_quote.clone()));
    assert!(!after_second.persisted_draft.contains("first authority"));

    let projected = controller.side_chat_projection();
    assert_eq!(projected.draft_text, second_text);
    assert!(
        !projected
            .draft_text
            .starts_with(super::super::side_chat::SIDE_CHAT_DRAFT_ENVELOPE_PREFIX)
    );
    assert_eq!(
        projected
            .draft_quote
            .as_ref()
            .map(|quote| quote.source_history_item_id.clone()),
        Some(second_quote.source_history_item_id.to_string())
    );

    let paths = controller.app.store.paths().clone();
    drop(controller);
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("reopen sqlite");
    sqlite.migrate().expect("audit migrations on reopen");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let args = DesktopArgs {
        directory: Some(root),
        session_id: Some(owner_session_id),
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut reopened = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("reopen controller");
    let restarted_projection = reopened.side_chat_projection();
    assert_eq!(restarted_projection.draft_text, second_text);
    assert_eq!(
        restarted_projection.draft_quote,
        Some(DesktopSideChatDraftQuoteProjection {
            source_kind: "artifact".to_string(),
            source_history_item_id: second_quote.source_history_item_id.to_string(),
            source_append_position: Some("42".to_string()),
            selected_text: "second authority".to_string(),
        })
    );

    reopened
        .save_side_chat_draft(
            owner_session_id,
            after_second.id,
            after_second.draft_revision,
            "plain text after manual edit".to_string(),
            None,
        )
        .expect("manual edit clears durable quote");
    let plain = reopened
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("plain binding read")
        .expect("plain binding");
    assert_eq!(plain.persisted_draft, "plain text after manual edit");
    let plain_projection = reopened.side_chat_projection();
    assert_eq!(plain_projection.draft_text, "plain text after manual edit");
    assert_eq!(plain_projection.draft_quote, None);
}

#[tokio::test]
async fn malformed_side_chat_draft_envelope_is_hidden_blocked_and_recoverable() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:9".to_string(),
            "never-contact-provider".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let initial = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("initial binding read")
        .expect("initial binding");
    let quote = SideChatQuoteRequest {
        source_kind: super::super::side_chat::SideChatQuoteSourceKind::Transcript,
        source_history_item_id: crate::protocol::HistoryItemId::new(),
        source_append_position: Some(7),
        selected_text: "must remain opaque".to_string(),
    };
    let mut malformed =
        encode_persisted_side_chat_draft("must not reach the provider".to_string(), Some(&quote))
            .expect("valid envelope before corruption");
    assert_eq!(malformed.pop(), Some('}'));
    let corrupt = controller
        .app
        .store
        .side_chat_repo()
        .update_draft(
            owner_session_id,
            initial.id,
            initial.draft_revision,
            malformed.clone(),
        )
        .expect("seed corrupt envelope")
        .binding;

    let projection = controller.side_chat_projection();
    assert_eq!(projection.draft_text, "");
    assert_eq!(projection.draft_quote, None);
    assert!(!projection.can_send);
    assert_eq!(
        projection.last_error,
        "the saved Side Chat draft is invalid; edit and save the draft to recover"
    );
    assert!(
        !projection
            .last_error
            .contains("must not reach the provider")
    );

    let error = controller
        .start_side_chat(
            owner_session_id,
            corrupt.id,
            corrupt.request_generation,
            corrupt.draft_revision,
            None,
            None,
            malformed,
        )
        .expect_err("malformed durable envelope must stop before provider transport");
    assert_eq!(
        error,
        "the saved Side Chat draft is invalid; edit and save the draft to recover"
    );
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));

    controller
        .save_side_chat_draft(
            owner_session_id,
            corrupt.id,
            corrupt.draft_revision,
            "recovered plain draft".to_string(),
            None,
        )
        .expect("overwrite corrupt envelope");
    let recovered = controller.side_chat_projection();
    assert_eq!(recovered.draft_text, "recovered plain draft");
    assert_eq!(recovered.draft_quote, None);
    assert!(recovered.can_send);
}

#[tokio::test]
async fn stale_side_chat_owner_snapshot_is_rejected_before_hidden_admission_or_provider_post() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:9".to_string(),
            "never-contact-provider".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let owner_item = crate::protocol::HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: owner_session_id,
        scope: crate::protocol::HistoryScope::Turn {
            turn_id: TurnId::new(),
        },
        sequence_no: 0,
        created_at_ms: 0,
        payload: crate::protocol::HistoryItemPayload::UserTurn {
            content: vec![crate::protocol::ContentPart::Text {
                text: "new canonical owner evidence".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        },
    };
    controller
        .app
        .store
        .protocol_event_store()
        .seed_history_item_for_test(&owner_item)
        .expect("owner history");
    let current_fence = controller
        .app
        .store
        .protocol_event_store()
        .visit_active_history_pages_for_session(
            owner_session_id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |_| Ok(()),
        )
        .expect("owner fence")
        .append_fence;
    controller.side_chat_contexts.insert(
        owner_session_id,
        SideChatContextMetadata {
            owner_session_id,
            scope: "owner_session",
            as_of_append_position: Some(current_fence.unwrap_or(0).saturating_sub(1)),
            truncated: true,
            owner_unit_count: 1,
            included_owner_unit_count: 1,
            quote_source_history_item_id: None,
        },
    );
    let idle_projection = controller.side_chat_projection();
    assert_eq!(
        idle_projection.context_as_of_append_position,
        current_fence.map(|position| position.to_string()),
        "an idle Side Chat must advertise the current owner fence for its next send"
    );
    assert!(
        !idle_projection.context_truncated,
        "metadata from a completed request must not make the next request look truncated"
    );
    let stale_fence = Some(current_fence.unwrap_or(0).saturating_add(1));
    let binding = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");

    let error = controller
        .start_side_chat(
            owner_session_id,
            binding.id,
            binding.request_generation,
            binding.draft_revision,
            stale_fence,
            None,
            "must remain retryable".to_string(),
        )
        .expect_err("stale owner context must fail before provider transport");
    assert!(error.contains("owner context changed"));
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
    let preserved = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("preserved binding read")
        .expect("preserved binding");
    assert_eq!(preserved.request_generation, binding.request_generation);
    assert_eq!(preserved.draft_revision, binding.draft_revision);
    assert!(preserved.persisted_draft.is_empty());
    let hidden = controller
        .app
        .session_service
        .get_session(binding.conversation_session_id)
        .await
        .expect("hidden session");
    assert_eq!(hidden.status, SessionStatus::Idle);
    assert!(
        controller
            .app
            .store
            .protocol_event_store()
            .list_history_items_for_session(binding.conversation_session_id)
            .expect("hidden history")
            .is_empty()
    );
}

#[tokio::test]
async fn side_chat_start_ack_owns_process_and_canonical_admission() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("side provider");
    listener
        .set_nonblocking(true)
        .expect("nonblocking side provider");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("provider address")
    );
    let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            endpoint,
            "google/gemma-4-12b-qat".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let binding = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    let provider = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = accepted_tx.send(());
                    let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
                    drop(stream);
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if release_rx.try_recv().is_ok() || std::time::Instant::now() >= deadline {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("side provider accept failed: {error}"),
            }
        }
    });

    controller
        .start_side_chat(
            owner_session_id,
            binding.id,
            binding.request_generation,
            binding.draft_revision,
            None,
            None,
            "canonical side question".to_string(),
        )
        .expect("admitted side chat start");
    accepted_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("provider connection");

    let projection = controller
        .app
        .store
        .side_chat_repo()
        .conversation_projection(owner_session_id)
        .expect("conversation projection")
        .expect("durable conversation");
    assert_eq!(projection.binding.request_generation, 1);
    assert_eq!(projection.binding.persisted_draft, "");
    assert_eq!(projection.status, SessionStatus::Running);
    assert!(projection.messages.iter().any(|message| {
        message.role == crate::storage::SideChatConversationRole::User
            && message.content == "canonical side question"
    }));
    assert!(
        controller
            .app
            .store
            .active_runs()
            .is_active(binding.conversation_session_id)
    );
    assert!(
        controller
            .app
            .store
            .try_acquire_run_process_lease(binding.conversation_session_id)
            .is_err()
    );

    let active_turn_id = controller
        .app
        .store
        .active_runs()
        .active_turn_id(binding.conversation_session_id)
        .expect("registered side turn");
    assert_eq!(
        controller.app.store.active_runs().cancel_turn(
            binding.conversation_session_id,
            active_turn_id,
            TurnInterruptionCause::UserStop,
        ),
        crate::runtime::ActiveRunInterruptOutcome::Applied,
        "the registry and provider request must share one cancellation owner"
    );
    let _ = release_tx.send(());
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.side_chat_runs.contains_key(&owner_session_id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
    assert!(
        !controller
            .app
            .store
            .active_runs()
            .is_active(binding.conversation_session_id)
    );
    let _released_process_owner = controller
        .app
        .store
        .try_acquire_run_process_lease(binding.conversation_session_id)
        .expect("released side process owner");
    provider.join().expect("side provider thread");
}

#[tokio::test]
async fn active_side_chat_rejects_stale_owner_actions_then_deletes_durably() {
    use crate::session::SessionRepository as _;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("side provider");
    listener
        .set_nonblocking(true)
        .expect("nonblocking side provider");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("provider address")
    );
    let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller.state.hub_connection = Some(crate::hub::HubConnection::new(
        crate::hub::HubSettingsStore::new(
            Utf8PathBuf::from_path_buf(temp.path().join("hub-settings.json")).unwrap(),
        ),
    ));
    let idle = controller
        .next_web_state()
        .expect("idle route availability");
    let idle_hub = idle.hub.expect("Hub route settings");
    assert_eq!(idle_hub.side_chat_mode, crate::hub::HubRouteMode::Direct);
    assert!(idle_hub.can_change_side_chat_mode);
    assert!(!controller.hub_context_active(crate::hub::HubReviewContext::SideChat));
    controller
        .configure_side_chat(
            owner_session_id,
            endpoint,
            "google/gemma-4-12b-qat".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let initial = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    let conversation_session_id = initial.conversation_session_id;
    let provider = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = accepted_tx.send(());
                    let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
                    drop(stream);
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if release_rx.try_recv().is_ok() || std::time::Instant::now() >= deadline {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("side provider accept failed: {error}"),
            }
        }
    });

    controller
        .start_side_chat(
            owner_session_id,
            initial.id,
            initial.request_generation,
            initial.draft_revision,
            None,
            None,
            "delete this side conversation".to_string(),
        )
        .expect("admitted side chat start");
    accepted_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("provider connection");
    let admitted = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("admitted binding read")
        .expect("admitted binding");

    let other = controller
        .app
        .store
        .session_repo()
        .create_session(crate::session::NewSession {
            project_id: controller.app.workspace.project_id,
            title: "other selected session".to_string(),
            cwd: controller.app.workspace.root.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("other main session");
    let other_snapshot = load_snapshot_for_selection(&controller.app, Some(other.id))
        .await
        .expect("other owner snapshot");
    controller.state.replace_snapshot(other_snapshot);
    controller.state.app_state.current_session_id = Some(other.id);
    controller.state.app_state.current_session_title = other.title;

    let background = controller
        .next_web_state()
        .expect("background Side route availability");
    assert_ne!(background.side_chat.status, "running");
    let background_hub = background.hub.expect("Hub route settings");
    assert!(controller.hub_context_active(crate::hub::HubReviewContext::SideChat));
    assert_eq!(
        background_hub.can_change_side_chat_mode,
        !controller.hub_context_active(crate::hub::HubReviewContext::SideChat),
        "a background Direct Side run closes the same mode gate used by the command"
    );
    assert!(background_hub.can_change_main_mode);
    assert!(!controller.hub_context_active(crate::hub::HubReviewContext::Main));

    let cancel_error = controller
        .cancel_side_chat(owner_session_id, admitted.id, admitted.request_generation)
        .expect_err("stale owner must not stop an active side chat");
    assert!(cancel_error.contains("selected main session changed"));
    let delete_error = controller
        .delete_side_chat(owner_session_id, admitted.id, admitted.request_generation)
        .expect_err("stale owner must not tombstone an active side chat");
    assert!(delete_error.contains("selected main session changed"));
    assert!(
        !controller
            .side_chat_runs
            .get(&owner_session_id)
            .expect("active owner run")
            .cancel
            .is_cancelled(),
        "the stale Stop target must have no cancellation side effect"
    );
    assert_eq!(
        controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("unchanged binding read")
            .expect("unchanged binding")
            .delete_requested_at_ms,
        None,
        "the stale delete target must not persist a tombstone"
    );

    let owner_snapshot = load_snapshot_for_selection(&controller.app, Some(owner_session_id))
        .await
        .expect("restore owner snapshot");
    controller.state.replace_snapshot(owner_snapshot);
    controller.state.app_state.current_session_id = Some(owner_session_id);
    controller.state.app_state.current_session_title = "side chat owner".to_string();

    controller
        .delete_side_chat(owner_session_id, admitted.id, admitted.request_generation)
        .expect("accept active side deletion");

    let deleting = controller
        .app
        .store
        .side_chat_repo()
        .get_by_owner(owner_session_id)
        .expect("deleting binding read")
        .expect("deleting binding");
    assert!(
        deleting.delete_requested_at_ms.is_some(),
        "the destructive confirmation must be durable before provider cancellation settles"
    );
    let active = controller
        .side_chat_runs
        .get(&owner_session_id)
        .expect("side run remains owned while deletion finalizes");
    assert!(active.delete_after_finish);
    assert!(active.cancel.is_cancelled());

    let _ = release_tx.send(());
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.side_chat_runs.contains_key(&owner_session_id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
    let settled = controller
        .next_web_state()
        .expect("settled Side route availability");
    assert!(!controller.hub_context_active(crate::hub::HubReviewContext::SideChat));
    assert!(
        settled
            .hub
            .expect("Hub route settings")
            .can_change_side_chat_mode
    );
    assert!(
        controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("deleted binding read")
            .is_none()
    );
    assert!(
        controller
            .app
            .session_service
            .get_session(conversation_session_id)
            .await
            .is_err(),
        "the hidden canonical side conversation must be deleted"
    );
    controller
        .app
        .session_service
        .get_session(owner_session_id)
        .await
        .expect("main owner remains available");
    provider.join().expect("side provider thread");
}

#[tokio::test]
async fn side_chat_process_owner_failure_preserves_draft_and_generation() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:1234".to_string(),
            "google/gemma-4-12b-qat".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let repository = controller.app.store.side_chat_repo();
    let binding = repository
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    let draft = repository
        .update_draft(
            owner_session_id,
            binding.id,
            binding.draft_revision,
            "keep this draft",
        )
        .expect("persist draft")
        .binding;
    let process_owner = controller
        .app
        .store
        .try_acquire_run_process_lease(binding.conversation_session_id)
        .expect("competing process owner");

    let error = controller
        .start_side_chat(
            owner_session_id,
            binding.id,
            binding.request_generation,
            draft.draft_revision,
            None,
            None,
            "must not be admitted".to_string(),
        )
        .expect_err("competing process owner must reject start");

    assert!(error.contains("another live process"));
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
    let preserved = repository
        .get_by_owner(owner_session_id)
        .expect("preserved binding read")
        .expect("preserved binding");
    assert_eq!(preserved.request_generation, binding.request_generation);
    assert_eq!(preserved.persisted_draft, "keep this draft");
    assert_eq!(preserved.draft_revision, draft.draft_revision);
    let projection = repository
        .conversation_projection(owner_session_id)
        .expect("preserved conversation")
        .expect("conversation projection");
    assert_eq!(projection.status, SessionStatus::Idle);
    assert!(projection.messages.is_empty());
    drop(process_owner);
}

#[tokio::test]
async fn side_chat_submit_rejects_stale_draft_revision_before_starting_a_worker() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:1234".to_string(),
            "google/gemma-4-12b-qat".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let repository = controller.app.store.side_chat_repo();
    let binding = repository
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    let saved = repository
        .update_draft(
            owner_session_id,
            binding.id,
            binding.draft_revision,
            "newer durable draft",
        )
        .expect("save newer draft")
        .binding;

    let error = controller
        .start_side_chat(
            owner_session_id,
            binding.id,
            binding.request_generation,
            binding.draft_revision,
            None,
            None,
            "stale submitted text".to_string(),
        )
        .expect_err("stale draft revision must reject submit");

    assert!(error.contains("side chat draft changed"));
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
    let preserved = repository
        .get_by_owner(owner_session_id)
        .expect("preserved binding read")
        .expect("preserved binding");
    assert_eq!(preserved.request_generation, binding.request_generation);
    assert_eq!(preserved.persisted_draft, "newer durable draft");
    assert_eq!(preserved.draft_revision, saved.draft_revision);
}

#[tokio::test]
async fn side_chat_admission_failure_preserves_draft_and_generation() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:1234".to_string(),
            "google/gemma-4-12b-qat".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let side_chat_repository = controller.app.store.side_chat_repo();
    let binding = side_chat_repository
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    let draft = side_chat_repository
        .update_draft(
            owner_session_id,
            binding.id,
            binding.draft_revision,
            "keep this draft",
        )
        .expect("persist draft")
        .binding;
    let blocking_turn_id = TurnId::new();
    let blocking_user_turn = UserTurn {
        turn_id: blocking_turn_id,
        items: vec![UserInputItem::Text {
            text: "existing request".to_string(),
        }],
        prompt_dispatch: None,
        editor_context: None,
    };
    let blocking_admission = controller
        .app
        .store
        .session_repo()
        .admit_session_turn_with_initial_user_turn(
            binding.conversation_session_id,
            blocking_turn_id,
            Some(&blocking_user_turn),
        )
        .await
        .expect("blocking admission")
        .expect("admitted blocker");

    let error = controller
        .start_side_chat(
            owner_session_id,
            binding.id,
            binding.request_generation,
            draft.draft_revision,
            None,
            None,
            "must not replace the active turn".to_string(),
        )
        .expect_err("active canonical turn must reject admission");

    assert!(error.contains("could not admit request generation"));
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
    let preserved = side_chat_repository
        .get_by_owner(owner_session_id)
        .expect("preserved binding read")
        .expect("preserved binding");
    assert_eq!(preserved.request_generation, binding.request_generation);
    assert_eq!(preserved.persisted_draft, "keep this draft");
    assert_eq!(preserved.draft_revision, draft.draft_revision);
    let terminal = crate::session::DurableTurnTerminal {
        outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::UserStop,
        },
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    controller
        .app
        .store
        .session_repo()
        .terminalize_admitted_turn_with_protocol_event(
            binding.conversation_session_id,
            blocking_admission.admission_id,
            &RunEvent::TurnTerminal {
                session_id: binding.conversation_session_id,
                terminal: Box::new(terminal),
            },
            blocking_turn_id,
            None,
            None,
        )
        .await
        .expect("terminalize blocker");
}

#[tokio::test]
async fn side_chat_ack_timeout_retains_worker_until_late_admission_is_terminal() {
    let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
    controller
        .configure_side_chat(
            owner_session_id,
            "http://127.0.0.1:1234".to_string(),
            "google/gemma-4-12b-qat".to_string(),
            String::new(),
            ProviderProfile::OpenAiCompatible,
        )
        .expect("configure side chat");
    let side_chat_repository = controller.app.store.side_chat_repo();
    let binding = side_chat_repository
        .get_by_owner(owner_session_id)
        .expect("binding read")
        .expect("binding");
    let saved_draft = side_chat_repository
        .update_draft(
            owner_session_id,
            binding.id,
            binding.draft_revision,
            "late draft",
        )
        .expect("persist draft")
        .binding;
    let database_path = controller.app.store.paths().database_path.clone();
    let blocker = rusqlite::Connection::open(database_path.as_std_path())
        .expect("blocking sqlite connection");
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold sqlite writer");

    let error = controller
        .start_side_chat_with_ack_timeout(
            owner_session_id,
            binding.id,
            binding.request_generation,
            saved_draft.draft_revision,
            None,
            None,
            "late canonical question".to_string(),
            std::time::Duration::from_millis(50),
        )
        .expect_err("blocked admission must exceed the acknowledgement timeout");

    assert!(error.contains("did not acknowledge within 50ms"));
    let pending = controller
        .side_chat_runs
        .get(&owner_session_id)
        .expect("timed-out worker remains owned");
    assert!(pending.cancel.is_cancelled());
    assert_eq!(pending.run_generation, 1);
    blocker
        .execute_batch("COMMIT")
        .expect("release sqlite writer");

    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.side_chat_runs.contains_key(&owner_session_id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
    let projection = side_chat_repository
        .conversation_projection(owner_session_id)
        .expect("late conversation projection")
        .expect("late conversation");
    assert_eq!(projection.binding.request_generation, 1);
    assert_eq!(projection.binding.persisted_draft, "");
    assert_eq!(projection.status, SessionStatus::Cancelled);
    assert!(projection.messages.iter().any(|message| {
        message.role == crate::storage::SideChatConversationRole::User
            && message.content == "late canonical question"
    }));
    assert!(
        !controller
            .app
            .store
            .active_runs()
            .is_active(binding.conversation_session_id)
    );
    let _released_process_owner = controller
        .app
        .store
        .try_acquire_run_process_lease(binding.conversation_session_id)
        .expect("late side process owner released after terminal");
}

#[tokio::test]
async fn provider_local_context_apply_does_not_require_a_catalog_reload() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let baseline = controller
        .state
        .provider_config
        .effective_config
        .model
        .clone();
    let next_context_window = baseline.context_window.saturating_add(1);

    controller.accept_provider_action_input(
        baseline.base_url.clone(),
        baseline.provider_profile,
        baseline.api_key_env.clone().unwrap_or_default(),
        next_context_window.to_string(),
        baseline.model.clone(),
    );

    assert_eq!(
        controller.state.provider_config.provider_loaded_base_url,
        None
    );
    assert!(controller.state.can_apply_provider_selection());
    assert!(controller.apply_provider_session());
    let applied = &controller.state.provider_config.effective_config.model;
    assert_eq!(applied.context_window, next_context_window);
    assert_eq!(applied.max_output_tokens, baseline.max_output_tokens);
    assert_eq!(applied.extra_body_json, None);
}

#[tokio::test]
async fn provider_manual_target_apply_clears_hidden_provider_state_without_catalog_evidence() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .provider_config
        .effective_config
        .model
        .extra_headers
        .insert(
            "Authorization".to_string(),
            "must-not-cross-hosts".to_string(),
        );
    controller
        .state
        .provider_config
        .effective_config
        .model
        .extra_body_json = Some(serde_json::json!({
        "api_key": "must-not-cross-hosts-in-body",
        "num_ctx": 4096
    }));
    let baseline = controller
        .state
        .provider_config
        .effective_config
        .model
        .clone();

    controller.accept_provider_action_input(
        "http://127.0.0.1:8119".to_string(),
        ProviderProfile::OpenAiCompatible,
        String::new(),
        baseline.context_window.to_string(),
        baseline.model,
    );

    assert!(!controller.state.provider_catalog_owns_current_target());
    assert!(controller.state.can_apply_provider_selection());
    assert!(controller.apply_provider_session());
    let applied = &controller.state.provider_config.effective_config.model;
    assert_eq!(applied.base_url, "http://127.0.0.1:8119");
    assert_eq!(applied.provider_profile, ProviderProfile::OpenAiCompatible);
    assert!(applied.extra_headers.is_empty());
    assert_eq!(applied.extra_body_json, None);
}

#[tokio::test]
async fn provider_same_target_key_and_local_context_edits_drop_legacy_generation_state() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .provider_config
        .effective_config
        .model
        .extra_headers
        .insert("X-Provider-Tenant".to_string(), "tenant-a".to_string());
    controller
        .state
        .provider_config
        .effective_config
        .model
        .extra_body_json = Some(serde_json::json!({
        "num_ctx": 8192,
        "legacy_provider_option": true
    }));
    let baseline = controller
        .state
        .provider_config
        .effective_config
        .model
        .clone();

    controller.accept_provider_action_input(
        baseline.base_url,
        baseline.provider_profile,
        "OPENAI_API_KEY".to_string(),
        baseline.context_window.saturating_add(1).to_string(),
        baseline.model,
    );

    assert!(controller.apply_provider_session());
    let applied = &controller.state.provider_config.effective_config.model;
    assert_eq!(
        applied
            .extra_headers
            .get("X-Provider-Tenant")
            .map(String::as_str),
        Some("tenant-a")
    );
    assert_eq!(applied.extra_body_json, None);
}

#[tokio::test]
async fn provider_global_persistence_candidate_explicitly_clears_old_headers() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let mut global = controller.state.global_config().clone();
    global
        .model
        .extra_headers
        .insert("Authorization".to_string(), "old-host-secret".to_string());
    controller.state.replace_global_config(global.clone());
    let mut next = global;
    next.model.base_url = "http://127.0.0.1:8119/v1".to_string();
    next.model.provider_profile = ProviderProfile::OpenAiCompatible;
    next.model.extra_headers.clear();

    let candidate = controller
        .provider_config_persistence_candidate(&next)
        .expect("provider persistence candidate");
    let headers = candidate
        .fields
        .iter()
        .find(|field| field.key == ConfigField::ExtraHeadersJson)
        .expect("explicit extra headers field");
    assert_eq!(headers.value, "{}");
    assert!(
        headers.dirty,
        "the old header owner must be cleared on disk"
    );
}

#[tokio::test]
async fn provider_global_persistence_does_not_reapply_legacy_host_generation_fields() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let mut global = controller.state.global_config().clone();
    global.model.max_output_tokens = 4_096;
    global.model.temperature = Some(0.2);
    global.model.top_p = Some(0.8);
    global.model.top_k = Some(40);
    global.model.supports_reasoning = true;
    global.model.extra_body_json = Some(serde_json::json!({
        "chat_template_kwargs": { "enable_thinking": false }
    }));
    controller.state.replace_global_config(global.clone());
    let mut provider_selection = global.clone();
    provider_selection.model.max_output_tokens = 8_192;
    provider_selection.model.temperature = Some(0.9);
    provider_selection.model.top_p = Some(0.95);
    provider_selection.model.top_k = Some(80);
    provider_selection.model.supports_reasoning = false;
    provider_selection.model.extra_body_json = None;

    let candidate = controller
        .provider_config_persistence_candidate(&provider_selection)
        .expect("provider persistence candidate");
    let resolved = candidate
        .build_resolved_config(&global)
        .expect("resolved provider persistence candidate");
    let defaults = ResolvedConfig::default();

    assert_eq!(
        resolved.model.max_output_tokens,
        defaults.model.max_output_tokens
    );
    assert_eq!(resolved.model.temperature, defaults.model.temperature);
    assert_eq!(resolved.model.top_p, defaults.model.top_p);
    assert_eq!(resolved.model.top_k, defaults.model.top_k);
    assert_eq!(
        resolved.model.supports_reasoning,
        defaults.model.supports_reasoning
    );
    assert_eq!(
        resolved.model.extra_body_json,
        defaults.model.extra_body_json
    );
}

#[tokio::test]
async fn provider_local_context_apply_does_not_consume_stale_catalog_metadata() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let baseline = controller
        .state
        .provider_config
        .effective_config
        .model
        .clone();
    let stale_url = "http://127.0.0.1:4321".to_string();
    controller.accept_provider_action_input(
        stale_url.clone(),
        baseline.provider_profile,
        baseline.api_key_env.clone().unwrap_or_default(),
        baseline.context_window.to_string(),
        baseline.model.clone(),
    );
    controller
        .state
        .begin_provider_model_load(stale_url.clone());
    controller
        .state
        .finish_provider_model_load(vec![ProviderModelInfo {
            id: baseline.model.clone(),
            display_name: Some("stale catalog model".to_string()),
            context_window: Some(baseline.context_window.saturating_add(10)),
            max_output_tokens: Some(baseline.max_output_tokens.saturating_add(10)),
            supports_images: Some(!baseline.supports_images),
            supports_tools: Some(!baseline.supports_tools),
            supports_reasoning: Some(!baseline.supports_reasoning),
            max_parallel_predictions: Some(baseline.max_parallel_predictions.saturating_add(1)),
            load_state: crate::llm::ProviderModelLoadState::Unknown,
            source: "provider_catalog".to_string(),
        }]);
    assert!(controller.state.provider_catalog_owns_current_target());

    controller.state.show_provider_editor();
    assert!(!controller.state.provider_catalog_owns_current_target());
    controller.accept_provider_action_input(
        baseline.base_url.clone(),
        baseline.provider_profile,
        baseline.api_key_env.clone().unwrap_or_default(),
        baseline.context_window.saturating_add(1).to_string(),
        baseline.model.clone(),
    );

    assert!(controller.apply_provider_session());
    let applied = &controller.state.provider_config.effective_config.model;
    assert_eq!(applied.supports_images, baseline.supports_images);
    assert_eq!(applied.supports_tools, baseline.supports_tools);
    assert_eq!(applied.supports_reasoning, baseline.supports_reasoning);
    assert_eq!(
        applied.max_parallel_predictions,
        baseline.max_parallel_predictions
    );
}

#[tokio::test]
async fn provider_local_context_apply_preserves_metadata_from_the_effective_config() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let baseline = controller
        .state
        .provider_config
        .effective_config
        .model
        .clone();
    controller
        .state
        .begin_provider_model_load(baseline.base_url.clone());
    controller
        .state
        .finish_provider_model_load(vec![ProviderModelInfo {
            id: baseline.model.clone(),
            display_name: Some("current catalog model".to_string()),
            context_window: Some(baseline.context_window),
            max_output_tokens: Some(baseline.max_output_tokens),
            supports_images: Some(!baseline.supports_images),
            supports_tools: Some(!baseline.supports_tools),
            supports_reasoning: Some(!baseline.supports_reasoning),
            max_parallel_predictions: Some(baseline.max_parallel_predictions.saturating_add(1)),
            load_state: crate::llm::ProviderModelLoadState::Unknown,
            source: "provider_catalog".to_string(),
        }]);
    assert!(controller.state.provider_catalog_owns_current_target());
    controller.accept_provider_action_input(
        baseline.base_url.clone(),
        baseline.provider_profile,
        baseline.api_key_env.clone().unwrap_or_default(),
        baseline.context_window.saturating_add(1).to_string(),
        baseline.model.clone(),
    );

    assert!(controller.apply_provider_session());
    let applied = &controller.state.provider_config.effective_config.model;
    assert_eq!(applied.supports_images, baseline.supports_images);
    assert_eq!(applied.supports_tools, baseline.supports_tools);
    assert_eq!(applied.supports_reasoning, baseline.supports_reasoning);
    assert_eq!(
        applied.max_parallel_predictions,
        baseline.max_parallel_predictions
    );
}

#[tokio::test]
async fn finished_interruption_projects_cancelled_even_if_the_live_view_is_still_running() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
    controller.state.begin_agent_run();
    controller.run_lifecycle.begin(17, RunControl::new());
    controller.state.apply_run_event(&RunEvent::ToolCallFailed {
        tool_call_id: crate::session::ToolCallId::new(),
        tool: crate::tool::ToolName::Shell,
        error: "shell invocation failed before the interruption".to_string(),
        metadata: serde_json::json!({}),
    });
    let summary = RunSummary::from_terminal(
        session_id,
        crate::protocol::TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 4,
            failed_tool_count: 1,
            change_count: 0,
            metrics: crate::session::RunMetrics {
                model_request_count: 3,
                ..Default::default()
            },
        },
    );

    controller
        .runtime_tx
        .send(RuntimeMessage::Finished {
            run_generation: 17,
            result: Ok(summary),
        })
        .expect("durable interrupted finish");
    controller.drain_runtime_messages();

    assert!(!controller.run_lifecycle.root_is_active());
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Cancelled
    );
    assert_eq!(
        controller.state.app_state.interruption_cause,
        Some(crate::protocol::TurnInterruptionCause::UserStop)
    );
    assert_eq!(
        controller.state.status_code,
        super::super::state::DesktopStatusCode::UserStopped
    );
    assert_eq!(controller.state.app_state.progress.model_requests, 3);
    assert_eq!(controller.state.app_state.progress.tool_calls_started, 4);
    assert_eq!(controller.state.app_state.progress.tool_calls_failed, 1);
}

async fn background_session_stop_fixture() -> (
    tempfile::TempDir,
    DesktopController,
    SessionId,
    TurnId,
    crate::storage::session_repo::AdmittedTurnSnapshot,
) {
    use crate::session::{NewSession, SessionRepository as _};
    let (temp, root, controller) = empty_access_test_controller().await;
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "external Stop target".to_string(),
            cwd: root,
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    let turn_id = TurnId::new();
    let admission = controller
        .app
        .store
        .session_repo()
        .admit_session_turn(session.id, turn_id)
        .await
        .expect("admission")
        .expect("admitted");
    (temp, controller, session.id, turn_id, admission)
}

async fn finish_background_session_stop(
    app: &App,
    session_id: SessionId,
    turn_id: TurnId,
    admission: &crate::storage::session_repo::AdmittedTurnSnapshot,
) {
    app.store
        .session_repo()
        .terminalize_admitted_turn_with_protocol_event(
            session_id,
            admission.admission_id,
            &RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                        cause: crate::protocol::TurnInterruptionCause::UserStop,
                    },
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                }),
            },
            turn_id,
            None,
            None,
        )
        .await
        .expect("external terminal");
}

#[tokio::test]
async fn background_session_stop_waits_for_durable_terminal_before_snapshot() {
    let (_temp, controller, session_id, turn_id, admission) =
        background_session_stop_fixture().await;
    let expected = ActiveTurnExpectation::Turn {
        turn_id,
        revision: admission.admission_revision,
    };
    claim_exact_root_execution_stop(&controller.app, session_id, expected)
        .await
        .expect("request accepted");
    let mut confirmation = Box::pin(load_stopped_session_operation_projection(
        &controller.app,
        session_id,
        expected,
        std::time::Duration::from_secs(2),
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), confirmation.as_mut())
            .await
            .is_err(),
        "request acknowledgement must not complete the sidebar operation"
    );
    finish_background_session_stop(&controller.app, session_id, turn_id, &admission).await;
    let applied = confirmation.await.expect("terminal projection");
    let row = applied
        .snapshot
        .session_rows
        .iter()
        .find(|row| row.session_id == session_id)
        .expect("external row");
    assert_eq!(row.status, SessionStatus::Cancelled);
    assert_eq!(row.loaded_status, crate::session::LoadedSessionStatus::Idle);
    assert_eq!(row.active_turn_id, None);
    assert_eq!(applied.loaded.read.active_turn_id, None);
    assert_eq!(
        applied.message,
        format!("チャット {session_id} の実行が終了しました。")
    );
}

#[tokio::test]
async fn background_session_stop_timeout_preserves_active_state_and_reports_unconfirmed() {
    let (_temp, controller, session_id, turn_id, admission) =
        background_session_stop_fixture().await;
    let applied = load_stopped_session_operation_projection(
        &controller.app,
        session_id,
        ActiveTurnExpectation::Turn {
            turn_id,
            revision: admission.admission_revision,
        },
        std::time::Duration::ZERO,
    )
    .await
    .expect("bounded observation");
    assert_eq!(applied.loaded.read.active_turn_id, Some(turn_id));
    assert_eq!(applied.loaded.read.session.status, SessionStatus::Running);
    assert!(applied.message.contains("終了は未確認です"));
    assert!(applied.message.contains("一覧を更新"));
    finish_background_session_stop(&controller.app, session_id, turn_id, &admission).await;
}

#[tokio::test]
async fn background_session_stop_does_not_wait_for_or_cancel_a_replacement_admission() {
    let (_temp, controller, session_id, turn_id, admission) =
        background_session_stop_fixture().await;
    finish_background_session_stop(&controller.app, session_id, turn_id, &admission).await;
    let replacement = TurnId::new();
    let next = controller
        .app
        .store
        .session_repo()
        .admit_session_turn(session_id, replacement)
        .await
        .expect("replacement admission")
        .expect("replacement admitted");
    for expected in [
        ActiveTurnExpectation::Turn {
            turn_id,
            revision: admission.admission_revision,
        },
        ActiveTurnExpectation::Turn {
            turn_id: replacement,
            revision: admission.admission_revision,
        },
    ] {
        let applied = load_stopped_session_operation_projection(
            &controller.app,
            session_id,
            expected,
            std::time::Duration::ZERO,
        )
        .await
        .expect("replacement projection");
        assert_eq!(applied.loaded.read.active_turn_id, Some(replacement));
        assert_eq!(
            applied.loaded.read.admission_revision,
            next.admission_revision
        );
        assert!(applied.message.contains("新しい実行は継続中です"));
    }
    finish_background_session_stop(&controller.app, session_id, replacement, &next).await;
}

#[tokio::test]
async fn rejoined_running_session_observes_an_exact_terminal_committed_by_another_store() {
    use crate::protocol::ProtocolEventStore as _;
    use crate::session::{NewSession, SessionRepository as _};
    use crate::storage::session_repo::{ModelResponseWrite, PendingToolCallWrite};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let session = app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: app.workspace.project_id,
            title: "cross-process owner".to_string(),
            cwd: root.clone(),
            model: app.config.model.model.clone(),
            base_url: app.config.model.base_url.clone(),
            access_mode: app.config.permissions.access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    let turn_id = TurnId::new();
    let admission = app
        .store
        .session_repo()
        .admit_session_turn(session.id, turn_id)
        .await
        .expect("admission")
        .expect("turn admitted");
    let args = DesktopArgs {
        directory: Some(root),
        session_id: Some(session.id),
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");

    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Running
    );
    assert!(
        controller
            .session_runtime_listener
            .as_ref()
            .is_some_and(|listener| {
                listener.target.session_id == session.id && listener.target.turn_id == turn_id
            })
    );
    let listener = controller
        .session_runtime_listener
        .as_ref()
        .expect("listener")
        .generation;
    controller
        .runtime_tx
        .send(RuntimeMessage::CanonicalSessionEvent {
            listener_generation: listener.saturating_add(1),
            target: SessionRuntimeListenerTarget {
                workspace_root: controller.app.workspace.root.clone(),
                session_id: session.id,
                turn_id,
            },
            event: RuntimeEvent {
                id: crate::protocol::RuntimeEventId::new(),
                session_id: session.id,
                turn_id,
                sequence_no: 99,
                created_at_ms: 1,
                msg: RuntimeEventMsg::TurnTerminal {
                    terminal: Box::new(crate::session::DurableTurnTerminal {
                        outcome: crate::protocol::TurnTerminalOutcome::Failed {
                            error: "stale listener must not win".to_string(),
                        },
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    }),
                },
            },
        })
        .expect("stale listener message");
    controller.drain_runtime_messages();
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Running,
        "a stale listener generation cannot terminate the selected run"
    );
    let stale_turn_id = TurnId::new();
    controller
        .runtime_tx
        .send(RuntimeMessage::CanonicalSessionEvent {
            listener_generation: listener,
            target: SessionRuntimeListenerTarget {
                workspace_root: controller.app.workspace.root.clone(),
                session_id: session.id,
                turn_id: stale_turn_id,
            },
            event: RuntimeEvent {
                id: crate::protocol::RuntimeEventId::new(),
                session_id: session.id,
                turn_id: stale_turn_id,
                sequence_no: 1,
                created_at_ms: 1,
                msg: RuntimeEventMsg::TurnTerminal {
                    terminal: Box::new(crate::session::DurableTurnTerminal {
                        outcome: crate::protocol::TurnTerminalOutcome::Completed,
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    }),
                },
            },
        })
        .expect("stale turn message");
    controller.drain_runtime_messages();
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Running,
        "a stale listener turn cannot terminate the selected run"
    );

    let external_sqlite =
        crate::storage::SqliteStore::open(&paths).expect("external sqlite connection");
    let external_store = crate::storage::StoreBundle::new(external_sqlite);
    let failed_calls = [
        crate::session::ToolCallId::new(),
        crate::session::ToolCallId::new(),
    ];
    external_store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            session.id,
            admission.admission_id,
            turn_id,
            ModelResponseWrite {
                response_id: crate::protocol::ModelResponseId::new(),
                assistant_text: None,
                assistant_protocol_sequence_no: None,
                tool_calls: failed_calls
                    .iter()
                    .enumerate()
                    .map(|(index, id)| PendingToolCallWrite {
                        id: *id,
                        model_call_id: format!("external-failed-call-{index}"),
                        tool_name: "shell".to_string(),
                        arguments_json: serde_json::json!({"command": "python -m unittest"})
                            .to_string(),
                        protocol_sequence_no: None,
                    })
                    .collect(),
            },
        )
        .await
        .expect("external pending calls");
    for call in failed_calls {
        external_store
            .session_repo()
            .fail_tool_call_with_protocol_bundle(
                session.id,
                admission.admission_id,
                call,
                crate::tool::ToolName::Shell,
                "external shell invocation failed",
                serde_json::json!({}),
                turn_id,
                None,
            )
            .await
            .expect("external tool failure")
            .expect("external failed call settled");
    }
    let terminal = crate::session::DurableTurnTerminal {
        outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::UserStop,
        },
        final_response_id: None,
        tool_call_count: 2,
        failed_tool_count: 2,
        change_count: 0,
        metrics: Default::default(),
    };
    external_store
        .session_repo()
        .terminalize_admitted_turn_with_protocol_event(
            session.id,
            admission.admission_id,
            &RunEvent::TurnTerminal {
                session_id: session.id,
                terminal: Box::new(terminal.clone()),
            },
            turn_id,
            None,
            None,
        )
        .await
        .expect("external terminal commit");
    let canonical_terminal = external_store
        .protocol_event_store()
        .list_runtime_events(session.id, turn_id)
        .expect("canonical runtime events")
        .into_iter()
        .find(RuntimeEvent::is_terminal)
        .expect("canonical terminal");
    controller
        .app
        .session_event_hub
        .publisher()
        .publish(canonical_terminal.clone())
        .expect("live terminal wake");
    controller
        .app
        .session_event_hub
        .publisher()
        .publish(canonical_terminal)
        .expect("duplicate live wake is harmless");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while (controller.state.app_state.run_status == crate::tui::state::RunStatus::Running
        || controller.state.post_run_refresh_pending())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        controller.drain_runtime_messages();
    }

    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Cancelled
    );
    assert_eq!(
        controller.state.app_state.interruption_cause,
        Some(crate::protocol::TurnInterruptionCause::UserStop)
    );
    assert!(!controller.state.post_run_refresh_pending());
    assert_eq!(controller.state.app_state.progress.model_requests, 0);
    assert_eq!(controller.state.app_state.progress.tool_calls_started, 2);
    assert_eq!(controller.state.app_state.progress.tool_calls_failed, 2);
    assert!(controller.session_runtime_listener.is_none());
    assert_eq!(
        controller
            .state
            .app_state
            .transcript_entries
            .iter()
            .filter(|entry| entry.title == "Run interrupted")
            .count(),
        0,
        "the canonical refresh owns transcript rows; the lifecycle listener only projects state"
    );
}

fn loaded_test_session(
    controller: &DesktopController,
    root: &Utf8Path,
    session_id: SessionId,
    status: SessionStatus,
    cause: Option<TurnInterruptionCause>,
) -> LoadedSession {
    let session = SessionRecord {
        id: session_id,
        project_id: controller.app.workspace.project_id,
        title: "approval owner test".to_string(),
        status,
        cwd: root.to_path_buf(),
        model: controller.app.config.model.model.clone(),
        base_url: controller.app.config.model.base_url.clone(),
        access_mode: controller.app.config.permissions.access_mode,
        model_parameters: crate::session::SessionModelParameters::default(),
        provider_connection: None,
        session_settings_revision: 0,
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: matches!(
            status,
            SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
        )
        .then_some(2),
    };
    let turn_items = cause
        .map(|cause| crate::protocol::TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id: crate::protocol::TurnId::new(),
            source_item_id: None,
            sequence_no: 1,
            payload: crate::protocol::TurnItemPayload::Terminal {
                outcome: crate::protocol::TurnTerminalOutcome::Interrupted { cause },
            },
        })
        .into_iter()
        .collect::<Vec<_>>();
    let latest_turn_id = turn_items.last().map(|item| item.turn_id);
    LoadedSession {
        read: crate::session::CanonicalSessionRead {
            session: session.clone(),
            history: crate::session::CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: usize::MAX,
                total: 0,
                has_more: false,
                items: Vec::new(),
            },
            turns: crate::session::CanonicalTurnPage {
                session,
                offset: 0,
                limit: DESKTOP_TURN_PAGE_LIMIT,
                total: turn_items.len(),
                has_more: false,
                items: turn_items,
            },
            pending_turn_inputs: Vec::new(),
            turn_elapsed_ms: Default::default(),
            session_token_usage: Default::default(),
            turn_progress: None,
            latest_turn_id,
            active_turn_id: None,
            active_turn_sequence_no: None,
            admission_revision: u64::from(latest_turn_id.is_some()),
        },
        agent_activity_records: None,
    }
}

fn loaded_running_test_session(
    controller: &DesktopController,
    root: &Utf8Path,
    session_id: SessionId,
    turn_id: TurnId,
) -> LoadedSession {
    let mut loaded =
        loaded_test_session(controller, root, session_id, SessionStatus::Running, None);
    loaded.read.latest_turn_id = Some(turn_id);
    loaded.read.active_turn_id = Some(turn_id);
    loaded.read.active_turn_sequence_no = Some(0);
    loaded.read.admission_revision = 1;
    loaded
}

async fn assert_durable_image_conversation_projection(existing_conversation: bool) {
    use crate::protocol::{TurnItemPayload, UserInputItem, UserTurn};
    for image_count in 0..=2 {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let mut initial =
            loaded_test_session(&controller, &root, session_id, SessionStatus::Idle, None);
        let item = |turn_id, sequence_no, payload| crate::protocol::TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no,
            payload,
        };
        if existing_conversation {
            let previous = TurnId::new();
            initial.read.turns.items = vec![
                item(
                    previous,
                    1,
                    TurnItemPayload::UserMessage {
                        text: "previous request".into(),
                    },
                ),
                item(
                    previous,
                    2,
                    TurnItemPayload::AgentMessage {
                        text: "previous answer".into(),
                    },
                ),
            ];
            initial.read.turns.total = 2;
            initial.read.latest_turn_id = Some(previous);
        }
        controller.state.load_open_session(&initial.read);
        let prompt = "inspect the image\nand retain this second line";
        let images: Vec<_> = (0..image_count)
            .map(|index| crate::session::ImagePart {
                source_path: (index == 0).then(|| root.join("removed-image.png")),
                mime_type: "image/png".into(),
                data_base64: "aW1hZ2U=".into(),
                byte_len: 5,
            })
            .collect();
        if !existing_conversation {
            controller.state.composer.retained_images = images.clone();
        }
        let mut items = vec![UserInputItem::Text {
            text: prompt.into(),
        }];
        items.extend(
            images
                .into_iter()
                .map(|image| UserInputItem::Image { image }),
        );
        let stored = RunEvent::UserTurnStored {
            session_id,
            turn: Box::new(UserTurn {
                turn_id,
                items,
                prompt_dispatch: Some(crate::session::PromptDispatchPart::raw(prompt)),
                editor_context: None,
            }),
        };
        let user_item =
            crate::protocol::project_turn_item_for_run_event(&stored, session_id, turn_id, 1)
                .expect("canonical user message");
        let TurnItemPayload::UserMessage {
            text: expected_user,
        } = &user_item.payload
        else {
            unreachable!()
        };
        let expected_user = expected_user.clone();
        let generation = 41;
        controller
            .run_lifecycle
            .begin(generation, RunControl::new());
        controller.state.begin_agent_run();
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: generation,
            owner_workspace_path: root.clone(),
            owner_session_id: Some(session_id),
            prompt_dispatch: crate::session::PromptDispatchPart::raw(prompt),
            image_paths: Vec::new(),
            prompt_review_to_cancel: None,
        });
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: generation - 1,
                event: stored.clone(),
            })
            .unwrap();
        controller.drain_runtime_messages();
        assert!(
            controller.pending_root_submission.is_some(),
            "a stale run cannot acknowledge this draft"
        );
        assert_eq!(controller.composer_commit_generation, 0);
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: generation,
                event: stored.clone(),
            })
            .unwrap();
        controller.drain_runtime_messages();
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: generation,
                event: stored,
            })
            .unwrap();
        controller.drain_runtime_messages();
        assert_eq!(
            controller.composer_commit_generation, 1,
            "the durable receipt is consumed only once"
        );
        assert_eq!(
            controller
                .state
                .app_state
                .transcript_entries
                .iter()
                .filter(
                    |entry| entry.kind == crate::tui::state::TranscriptKind::User
                        && entry.body == expected_user
                )
                .count(),
            1,
            "duplicate receipts must not append or overwrite another user row"
        );
        controller.stop_session_runtime_listener();
        controller.run_lifecycle.finish_root();
        let terminal = crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        };
        controller
            .state
            .apply_run_summary(RunSummary::from_terminal(session_id, turn_id, terminal));
        let mut completed = loaded_test_session(
            &controller,
            &root,
            session_id,
            SessionStatus::Completed,
            None,
        );
        completed.read.turns.items = initial.read.turns.items;
        completed.read.turns.items.extend([
            user_item,
            item(
                turn_id,
                2,
                TurnItemPayload::AgentMessage {
                    text: "stored image answer".into(),
                },
            ),
            item(
                turn_id,
                3,
                TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            ),
        ]);
        completed.read.turns.total = completed.read.turns.items.len();
        completed.read.latest_turn_id = Some(turn_id);
        completed.read.admission_revision = 2;
        assert!(
            controller
                .state
                .load_open_session_preserving_history(&completed.read)
        );
        let detail = controller.state.selected_detail();
        assert!(
            detail
                .transcript_rows
                .iter()
                .any(|row| row.row_kind == DesktopTranscriptRowKind::Assistant
                    && row.body == "stored image answer"),
            "terminal canonical answer must be visible with {image_count} images"
        );
        assert_eq!(
            detail
                .transcript_rows
                .iter()
                .filter(|row| row.row_kind == DesktopTranscriptRowKind::User
                    && row.body == expected_user)
                .count(),
            1,
            "canonical user body must not be duplicated or dropped"
        );
    }
}

#[tokio::test]
async fn durable_image_user_projection_retains_answer_in_existing_conversation() {
    assert_durable_image_conversation_projection(true).await;
}

#[tokio::test]
async fn durable_image_user_projection_retains_answer_in_empty_edit_fork() {
    assert_durable_image_conversation_projection(false).await;
}

#[tokio::test]
async fn prompt_enhance_rejects_an_active_background_owner_mutation() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let operation_id = controller.state.begin_steer_submission();

    assert!(!controller.start_prompt_enhance("new review".to_string()));
    assert!(!controller.state.prompt_enhance_pending());
    assert!(controller.state.app_state.prompt_review.is_none());
    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some("prompt enhancement is not currently available")
    );

    assert!(controller.state.finish_steer_submission(operation_id));
}

#[tokio::test]
async fn prompt_review_operations_require_the_captured_idle_run_owner() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let turn_target = ActiveTurnExpectation::Turn {
        turn_id: TurnId::new(),
        revision: 0,
    };

    assert!(
        !controller.start_prompt_enhance_at("do not trap this review".to_string(), turn_target,)
    );
    assert!(!controller.state.prompt_enhance_pending());
    assert!(controller.state.app_state.prompt_review.is_none());

    assert!(
        !controller.start_review_uncommitted_at("review current changes".to_string(), turn_target,)
    );
    assert!(!controller.run_lifecycle.root_is_active());
    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some("uncommitted review requires the captured idle run owner")
    );

    controller.state.begin_prompt_enhance_at(
        77,
        "seeded Turn review",
        CancellationToken::new(),
        turn_target,
    );
    assert!(
        controller
            .state
            .finish_prompt_enhance(77, "seeded enhanced review".to_string())
    );
    assert!(!controller.send_prompt_review_at(
        77,
        true,
        "seeded enhanced review".to_string(),
        turn_target,
    ));
    let seeded_review_target = controller
        .prompt_review_target(77)
        .expect("seeded review target");
    assert!(!controller.launch_run_with_options(
        "seeded enhanced review".to_string(),
        crate::session::PromptDispatchPart::raw("seeded enhanced review"),
        None,
        Some(seeded_review_target),
        turn_target,
    ));
    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| review.request_id),
        Some(77),
        "a rejected Turn-owned review remains recoverable through exact cancel"
    );
    assert!(!controller.state.steer_submission_pending());
    assert!(!controller.run_lifecycle.root_is_active());
}

#[tokio::test]
async fn post_run_refresh_blocks_every_new_root_entrypoint_at_central_admission() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let expected_active_turn = controller.current_active_turn_expectation();
    let next_root_run_generation = controller.next_root_run_generation;
    controller.state.mark_post_run_refresh_pending();

    assert!(!controller.start_run_at("new root prompt".to_string(), expected_active_turn,));
    assert!(
        !controller.start_review_uncommitted_at(
            "review current changes".to_string(),
            expected_active_turn,
        )
    );

    controller.state.begin_prompt_enhance_at(
        78,
        "review source",
        CancellationToken::new(),
        expected_active_turn,
    );
    assert!(
        controller
            .state
            .finish_prompt_enhance(78, "reviewed prompt".to_string())
    );
    assert!(!controller.send_prompt_review_at(
        78,
        true,
        "reviewed prompt".to_string(),
        expected_active_turn,
    ));

    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some("wait for the completed task to finish refreshing before sending")
    );
    assert!(controller.state.post_run_refresh_pending());
    assert!(!controller.run_lifecycle.root_is_active());
    assert_eq!(
        controller.next_root_run_generation,
        next_root_run_generation
    );
    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| review.request_id),
        Some(78),
        "rejected review send keeps the exact review recoverable"
    );
}

#[tokio::test]
async fn prompt_review_central_admission_preserves_owner_and_draft_for_every_mutation_class() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .begin_prompt_enhance(78, "review source", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(78, "review draft".to_string())
    );

    for mutation_class in [
        "provider model loading",
        "provider configuration",
        "configuration",
        "configuration import",
        "access mode",
        "workspace navigation",
        "workspace picker",
        "overlay replacement",
        "image attachment",
    ] {
        assert!(!controller.ensure_unscoped_prompt_review_action(mutation_class));
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| (
                    review.request_id,
                    review.raw_prompt_text.as_str(),
                    review.current_draft_text.as_str(),
                )),
            Some((78, "review source", "review draft")),
            "{mutation_class} must reject without consuming the review owner",
        );
        assert_eq!(
            controller.state.prompt_review_expected_active_turn(78),
            Some(ActiveTurnExpectation::initial_idle()),
        );
        assert_eq!(
            controller.state.view.overlay,
            super::super::state::DesktopOverlay::PromptReview,
        );
    }
}

#[tokio::test]
async fn steer_settlement_has_no_prompt_review_cancellation_capability() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.rebind_composer_owner(Some(session_id));
    controller
        .state
        .begin_prompt_enhance(41, "review A", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(41, "enhanced A".to_string())
    );
    let steer_target = SteerSubmissionTarget {
        operation_id: controller.state.begin_steer_submission(),
        workspace_root: controller.app.workspace.root.clone(),
        session_id,
        expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
    };

    controller.state.cancel_prompt_review();
    controller
        .state
        .begin_prompt_enhance(42, "review B", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(42, "enhanced B".to_string())
    );
    controller
        .control_tx
        .send(RuntimeMessage::SteerFinished {
            target: steer_target,
            image_paths: Vec::new(),
            result: Ok(()),
        })
        .expect("stale review A steer settlement");

    controller.drain_runtime_messages();

    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| (review.request_id, review.current_draft_text.as_str())),
        Some((42, "enhanced B"))
    );
    assert!(!controller.state.steer_submission_pending());
}

#[tokio::test]
async fn stale_root_commit_cannot_cancel_a_newer_prompt_review() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .begin_prompt_enhance(51, "review A", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(51, "enhanced A".to_string())
    );
    let stale_review_target = controller
        .prompt_review_target(51)
        .expect("review A target");
    controller.pending_root_submission = Some(PendingRootSubmission {
        run_generation: 91,
        owner_workspace_path: controller.root_submission_owner_workspace_path(),
        owner_session_id: None,
        prompt_dispatch: crate::session::PromptDispatchPart::raw("review A"),
        image_paths: Vec::new(),
        prompt_review_to_cancel: Some(stale_review_target),
    });

    controller.state.cancel_prompt_review();
    controller
        .state
        .begin_prompt_enhance(52, "review B", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(52, "enhanced B".to_string())
    );

    assert!(controller.commit_pending_root_submission(91, None));
    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| (review.request_id, review.current_draft_text.as_str())),
        Some((52, "enhanced B"))
    );
}

#[tokio::test]
async fn first_session_adoption_cancels_the_exact_submitted_prompt_review() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    controller
        .state
        .begin_prompt_enhance(61, "first review", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(61, "first enhanced".to_string())
    );
    let review_target = controller
        .prompt_review_target(61)
        .expect("first-session review target");
    controller.pending_root_submission = Some(PendingRootSubmission {
        run_generation: 92,
        owner_workspace_path: controller.root_submission_owner_workspace_path(),
        owner_session_id: None,
        prompt_dispatch: crate::session::PromptDispatchPart::raw("first review"),
        image_paths: Vec::new(),
        prompt_review_to_cancel: Some(review_target),
    });
    let adopted_session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(adopted_session_id);

    assert!(controller.commit_pending_root_submission(92, None));
    assert!(controller.state.app_state.prompt_review.is_none());
    assert!(controller.state.composer.is_owned_by(
        &controller.state.snapshot.workspace_path,
        Some(adopted_session_id)
    ));
}

#[tokio::test]
async fn accepted_running_live_refresh_attaches_one_exact_runtime_listener() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    let target = SessionRefreshRequestTarget {
        workspace_root: root.clone(),
        session_id,
    };
    let first_request = controller
        .session_projection_refresh_requests
        .begin(target.clone());
    controller
        .runtime_tx
        .send(RuntimeMessage::LiveSessionRefreshed {
            request_id: first_request,
            target: target.clone(),
            result: Ok(loaded_running_test_session(
                &controller,
                &root,
                session_id,
                turn_id,
            )),
        })
        .expect("first running refresh");
    controller.drain_runtime_messages();

    let listener = controller
        .session_runtime_listener
        .as_ref()
        .expect("accepted Running projection must attach its canonical cursor");
    assert_eq!(listener.target.session_id, session_id);
    assert_eq!(listener.target.turn_id, turn_id);
    let generation = listener.generation;
    let cancel = listener.cancel.clone();

    let duplicate_request = controller
        .session_projection_refresh_requests
        .begin(target.clone());
    controller
        .runtime_tx
        .send(RuntimeMessage::LiveSessionRefreshed {
            request_id: duplicate_request,
            target,
            result: Ok(loaded_running_test_session(
                &controller,
                &root,
                session_id,
                turn_id,
            )),
        })
        .expect("duplicate running refresh");
    controller.drain_runtime_messages();

    assert_eq!(
        controller
            .session_runtime_listener
            .as_ref()
            .map(|listener| listener.generation),
        Some(generation),
        "the same canonical target must reuse its listener generation"
    );
    assert!(!cancel.is_cancelled());
}

#[tokio::test]
async fn admitted_user_turn_attaches_its_exact_cursor_without_waiting_for_refresh() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let run_generation = 27;
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    controller
        .run_lifecycle
        .begin(run_generation, RunControl::new());
    controller.state.begin_agent_run();
    controller
        .control_tx
        .send(RuntimeMessage::RunEvent {
            run_generation,
            event: RunEvent::SessionStarted {
                session_id,
                title: "new run".to_string(),
            },
        })
        .expect("session bootstrap");
    controller
        .control_tx
        .send(RuntimeMessage::RunEvent {
            run_generation,
            event: RunEvent::UserTurnStored {
                session_id,
                turn: Box::new(UserTurn {
                    turn_id,
                    items: vec![UserInputItem::Text {
                        text: "attach the durable cursor".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                }),
            },
        })
        .expect("durable admission bootstrap");

    controller.drain_runtime_messages();

    assert!(
        controller
            .session_runtime_listener
            .as_ref()
            .is_some_and(|listener| {
                listener.target.workspace_root == controller.app.workspace.root
                    && listener.target.session_id == session_id
                    && listener.target.turn_id == turn_id
            }),
        "UserTurnStored must close the listener gap before an async session refresh settles"
    );
}

#[tokio::test]
async fn runtime_listener_generation_rejects_target_aba() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    let first_turn_id = TurnId::new();
    let other_turn_id = TurnId::new();
    let first_target = SessionRuntimeListenerTarget {
        workspace_root: controller.app.workspace.root.clone(),
        session_id,
        turn_id: first_turn_id,
    };
    let other_target = SessionRuntimeListenerTarget {
        workspace_root: controller.app.workspace.root.clone(),
        session_id,
        turn_id: other_turn_id,
    };
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;

    controller.reconcile_session_runtime_listener(Some(first_target.clone()));
    let first_generation = controller
        .session_runtime_listener
        .as_ref()
        .expect("first listener")
        .generation;
    let first_cancel = controller
        .session_runtime_listener
        .as_ref()
        .expect("first listener")
        .cancel
        .clone();
    controller.reconcile_session_runtime_listener(Some(other_target));
    assert!(first_cancel.is_cancelled());
    let other_cancel = controller
        .session_runtime_listener
        .as_ref()
        .expect("replacement listener")
        .cancel
        .clone();
    controller.reconcile_session_runtime_listener(Some(first_target.clone()));
    assert!(other_cancel.is_cancelled());
    let current_generation = controller
        .session_runtime_listener
        .as_ref()
        .expect("re-adopted first target")
        .generation;
    assert!(current_generation > first_generation);

    controller
        .runtime_tx
        .send(RuntimeMessage::CanonicalSessionEvent {
            listener_generation: first_generation,
            target: first_target,
            event: RuntimeEvent {
                id: crate::protocol::RuntimeEventId::new(),
                session_id,
                turn_id: first_turn_id,
                sequence_no: 1,
                created_at_ms: 1,
                msg: RuntimeEventMsg::TurnTerminal {
                    terminal: Box::new(crate::session::DurableTurnTerminal {
                        outcome: crate::protocol::TurnTerminalOutcome::Completed,
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    }),
                },
            },
        })
        .expect("stale ABA listener event");
    controller.drain_runtime_messages();

    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Running
    );
    assert_eq!(
        controller
            .session_runtime_listener
            .as_ref()
            .map(|listener| listener.generation),
        Some(current_generation)
    );
}

#[tokio::test]
async fn worker_finished_overtakes_a_stale_running_refresh_and_leaves_no_listener() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let run_generation = 28;
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.begin_agent_run();
    controller
        .run_lifecycle
        .begin(run_generation, RunControl::new());
    let target = SessionRefreshRequestTarget {
        workspace_root: root.clone(),
        session_id,
    };
    let stale_request = controller
        .session_projection_refresh_requests
        .begin(target.clone());
    controller
        .runtime_tx
        .send(RuntimeMessage::LiveSessionRefreshed {
            request_id: stale_request,
            target,
            result: Ok(loaded_running_test_session(
                &controller,
                &root,
                session_id,
                turn_id,
            )),
        })
        .expect("queued stale running refresh");
    controller
        .control_tx
        .send(RuntimeMessage::Finished {
            run_generation,
            result: Ok(RunSummary::from_terminal(
                session_id,
                turn_id,
                crate::session::DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                },
            )),
        })
        .expect("lossless worker settlement");

    controller.drain_runtime_messages();

    assert!(!controller.run_lifecycle.root_is_active());
    assert!(controller.session_runtime_listener.is_none());
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Completed
    );
}

#[tokio::test]
async fn current_stop_request_refresh_applies_once_without_a_new_root_admission() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
    controller.state.mark_post_run_refresh_pending();
    let target = SessionRefreshRequestTarget {
        workspace_root: root.clone(),
        session_id,
    };
    let request_id = controller
        .session_projection_refresh_requests
        .begin(target.clone());
    let root_admission_fence = controller.next_root_run_generation;

    controller
        .runtime_tx
        .send(RuntimeMessage::CurrentSessionRefreshed {
            request_id,
            target: target.clone(),
            purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                root_admission_fence,
                root_stop_attempt: None,
                expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                in_memory_stop_accepted: true,
                durable_outcome: Some(ExactRootExecutionStopOutcome::Applied { cancelled: true }),
            },
            result: Ok(loaded_test_session(
                &controller,
                &root,
                session_id,
                SessionStatus::Completed,
                None,
            )),
        })
        .expect("current Stop settlement");
    controller.drain_runtime_messages();

    assert!(!controller.session_projection_refresh_requests.is_pending());
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Completed
    );
    assert!(!controller.state.post_run_refresh_pending());
    let settled_status = controller.state.app_state.status_message.clone();

    controller
        .runtime_tx
        .send(RuntimeMessage::CurrentSessionRefreshed {
            request_id,
            target,
            purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                root_admission_fence,
                root_stop_attempt: None,
                expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                in_memory_stop_accepted: true,
                durable_outcome: Some(ExactRootExecutionStopOutcome::Applied { cancelled: true }),
            },
            result: Err("duplicate Stop settlement".to_string()),
        })
        .expect("duplicate Stop settlement");
    controller.drain_runtime_messages();

    assert_eq!(controller.state.app_state.status_message, settled_status);
    assert!(!controller.session_projection_refresh_requests.is_pending());
}

#[tokio::test]
async fn running_stop_refresh_waits_for_the_matching_interrupted_summary() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    let run_generation = controller.next_root_run_generation;
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
    controller.state.begin_agent_run();
    controller
        .run_lifecycle
        .begin(run_generation, RunControl::new());
    controller.state.mark_run_stop_requested(
        "run cancellation requested",
        "停止を要求しました。現在の処理を中断しています。",
    );
    controller.state.mark_post_run_refresh_pending();
    let target = SessionRefreshRequestTarget {
        workspace_root: root.clone(),
        session_id,
    };
    let request_id = controller
        .session_projection_refresh_requests
        .begin(target.clone());

    controller
        .runtime_tx
        .send(RuntimeMessage::CurrentSessionRefreshed {
            request_id,
            target,
            purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                root_admission_fence: controller.next_root_run_generation,
                root_stop_attempt: None,
                expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                in_memory_stop_accepted: true,
                durable_outcome: Some(ExactRootExecutionStopOutcome::Applied { cancelled: true }),
            },
            result: Ok(loaded_test_session(
                &controller,
                &root,
                session_id,
                SessionStatus::Running,
                None,
            )),
        })
        .expect("Stop acknowledgement");
    controller.drain_runtime_messages();

    assert!(controller.run_lifecycle.root_is_active());
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Running
    );
    assert_eq!(controller.state.app_state.progress.status, "Stopping");
    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some("Stop request accepted; waiting for the matching terminal event")
    );

    let summary = RunSummary::from_terminal(
        session_id,
        crate::protocol::TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 1,
            failed_tool_count: 0,
            change_count: 0,
            metrics: crate::session::RunMetrics {
                model_request_count: 2,
                ..Default::default()
            },
        },
    );
    controller
        .runtime_tx
        .send(RuntimeMessage::Finished {
            run_generation,
            result: Ok(summary),
        })
        .expect("matching interrupted summary");
    controller.drain_runtime_messages();

    assert!(!controller.run_lifecycle.root_is_active());
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Cancelled
    );
    assert_eq!(
        controller.state.status_code,
        super::super::state::DesktopStatusCode::UserStopped
    );
}

#[tokio::test]
async fn target_changed_stop_refresh_loads_b_without_marking_b_stopping() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    let turn_a = TurnId::new();
    let turn_b = TurnId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.begin_agent_run();
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
    controller.state.mark_post_run_refresh_pending();
    let target = SessionRefreshRequestTarget {
        workspace_root: root.clone(),
        session_id,
    };
    let request_id = controller
        .session_projection_refresh_requests
        .begin(target.clone());

    controller
        .runtime_tx
        .send(RuntimeMessage::CurrentSessionRefreshed {
            request_id,
            target,
            purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                root_admission_fence: controller.next_root_run_generation,
                root_stop_attempt: None,
                expected_active_turn: Some(ActiveTurnExpectation::Turn {
                    turn_id: turn_a,
                    revision: 1,
                }),
                in_memory_stop_accepted: true,
                durable_outcome: Some(ExactRootExecutionStopOutcome::TargetChanged),
            },
            result: Ok(loaded_running_test_session(
                &controller,
                &root,
                session_id,
                turn_b,
            )),
        })
        .expect("TargetChanged Stop refresh");
    controller.drain_runtime_messages();

    assert_eq!(
        controller.current_active_turn_expectation(),
        ActiveTurnExpectation::Turn {
            turn_id: turn_b,
            revision: 1,
        }
    );
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Running
    );
    assert_ne!(controller.state.app_state.progress.status, "Stopping");
    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some("the task changed before Stop was applied; the replacement was not stopped")
    );
}

#[tokio::test]
async fn turn_a_stop_dispatches_after_the_controller_projects_terminal_latest_a() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    let turn_a = TurnId::new();
    let mut loaded = loaded_test_session(
        &controller,
        &root,
        session_id,
        SessionStatus::Completed,
        None,
    );
    loaded.read.latest_turn_id = Some(turn_a);
    loaded.read.admission_revision = 1;
    controller.state.load_open_session(&loaded.read);
    assert_eq!(
        controller.current_active_turn_expectation(),
        ActiveTurnExpectation::Idle {
            latest_turn_id: Some(turn_a),
            revision: 1,
        }
    );

    controller.cancel_exact_turn_at(turn_a, 1);

    assert!(controller.session_projection_refresh_requests.is_pending());
    assert!(controller.state.post_run_refresh_pending());
    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some("validating the exact task Stop target...")
    );
}

#[tokio::test]
async fn stop_request_refresh_from_before_the_next_root_admission_cannot_mutate_that_root() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    let target = SessionRefreshRequestTarget {
        workspace_root: root.clone(),
        session_id,
    };
    let request_id = controller
        .session_projection_refresh_requests
        .begin(target.clone());
    let old_root_admission_fence = controller.next_root_run_generation;

    controller.next_root_run_generation = old_root_admission_fence + 1;
    controller
        .run_lifecycle
        .begin(old_root_admission_fence, RunControl::new());
    controller.state.begin_agent_run();
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
    controller.state.composer.draft_prompt = "new root draft".to_string();
    controller
        .state
        .begin_prompt_enhance(91, "new root review source", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(91, "new root review".to_string())
    );
    controller.state.mark_post_run_refresh_pending();
    controller
        .state
        .set_status_message("new root terminal refresh pending");

    controller
        .runtime_tx
        .send(RuntimeMessage::CurrentSessionRefreshed {
            request_id,
            target,
            purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                root_admission_fence: old_root_admission_fence,
                root_stop_attempt: None,
                expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                in_memory_stop_accepted: true,
                durable_outcome: Some(ExactRootExecutionStopOutcome::Applied { cancelled: true }),
            },
            result: Ok(loaded_test_session(
                &controller,
                &root,
                session_id,
                SessionStatus::Completed,
                None,
            )),
        })
        .expect("stale Stop settlement");
    controller.drain_runtime_messages();

    assert!(!controller.session_projection_refresh_requests.is_pending());
    assert_eq!(
        controller.run_lifecycle.root_generation(),
        Some(old_root_admission_fence)
    );
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Running
    );
    assert_eq!(controller.state.composer.draft_prompt, "new root draft");
    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| review.current_draft_text.as_str()),
        Some("new root review")
    );
    assert!(controller.state.post_run_refresh_pending());
    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some("new root terminal refresh pending")
    );
}

#[tokio::test]
async fn detached_child_permission_survives_root_finish_and_ordinary_refresh() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Completed;
    let request = test_permission("detached-child");
    let expected_path = request.agent_path.clone();
    let expected_task = request.agent_task_name.clone();
    let (response, receiver) = mpsc::channel();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request,
        responder: response,
        run_control: RunControl::new(),
    });

    controller.settle_pending_permission_after_root_finish();
    controller.apply_current_session_refreshed_message(
        session_id,
        CurrentSessionRefreshPurpose::Refresh,
        Ok(loaded_test_session(
            &controller,
            &root,
            session_id,
            SessionStatus::Completed,
            None,
        )),
    );

    let projection = controller.next_web_state().expect("permission projection");
    assert_eq!(projection.confirmation_id.as_deref(), Some("42"));
    assert!(projection.confirmation_visible);
    let confirmation = projection.confirmation.expect("confirmation requester");
    assert_eq!(confirmation.agent_path, expected_path);
    assert_eq!(confirmation.agent_task_name, expected_task);
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Completed
    );

    assert_eq!(
        controller.answer_permission(42, ReviewDecision::Approved),
        PendingPermissionResolution::Resolved
    );
    assert_eq!(receiver.try_recv(), Ok(ReviewDecision::Approved));
}

#[tokio::test]
async fn terminal_child_permission_does_not_survive_root_finish_without_cancel_event() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let request = test_permission("terminal-child");
    let (response, receiver) = mpsc::channel();
    let run_control = RunControl::new();
    assert!(run_control.interrupt(TurnInterruptionCause::TreeStopped));
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request,
        responder: response,
        run_control,
    });

    controller.settle_pending_permission_after_root_finish();

    assert!(controller.pending_permission.is_none());
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    assert!(
        !controller
            .next_web_state()
            .expect("projection")
            .confirmation_visible
    );
}

#[tokio::test]
async fn session_reselection_preserves_durable_interruption_presentation() {
    for cause in [
        TurnInterruptionCause::ApprovalAborted,
        TurnInterruptionCause::UserStop,
    ] {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        let request_id = controller.state.begin_session_load(session_id);
        let loaded = loaded_test_session(
            &controller,
            &root,
            session_id,
            SessionStatus::Cancelled,
            Some(cause),
        );
        let target = SessionLoadRequestTarget {
            workspace_root: controller.app.workspace.root.clone(),
            workspace_cwd: controller.app.workspace.cwd.clone(),
            project_id: controller.app.workspace.project_id,
            session_id,
        };

        controller.apply_session_loaded_message(
            request_id,
            target,
            SessionLoadReason::UserSelection,
            Ok(SessionNavigationLoadResult {
                workspace: None,
                snapshot: None,
                loaded,
            }),
        );

        let expected = crate::tui::state::interruption_status_message(cause);
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some(expected.as_str())
        );
        assert!(
            !controller
                .state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(|message| message == "会話を開きました。")
        );
        let projection = controller.next_web_state().expect("rehydrated projection");
        assert_eq!(projection.status_message, expected);
    }
}

#[tokio::test]
async fn created_edit_fork_refreshes_same_workspace_sidebar_and_selects_exact_session() {
    use crate::session::{NewSession, SessionRepository as _};

    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let create = |title: &str| NewSession {
        project_id: controller.app.workspace.project_id,
        title: title.to_string(),
        cwd: root.clone(),
        model: controller.app.config.model.model.clone(),
        base_url: controller.app.config.model.base_url.clone(),
        access_mode: controller.app.config.permissions.access_mode,
        provider_connection: None,
    };
    let original = controller
        .app
        .store
        .session_repo()
        .create_session(create("Original"))
        .await
        .expect("original session");
    let old_snapshot = load_snapshot_for_selection(&controller.app, Some(original.id))
        .await
        .expect("old sidebar snapshot");
    controller.state.replace_snapshot(old_snapshot);
    controller.state.app_state.current_session_id = Some(original.id);

    let fork = controller
        .app
        .store
        .session_repo()
        .create_session(create("Original (edit)"))
        .await
        .expect("new fork session");
    assert!(
        !controller
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == fork.id)
    );
    let retained_image = crate::session::ImagePart {
        source_path: Some(root.join("deleted-original.png")),
        mime_type: "image/png".into(),
        data_base64: "saved-image-bytes".into(),
        byte_len: 1,
    };
    controller.edit_image_drafts.insert(
        fork.id,
        (controller.app.workspace.project_id, vec![retained_image]),
    );

    let result = load_session_navigation_result(
        controller.app.clone(),
        fork.id,
        SessionLoadReason::CreatedEditFork,
    )
    .await
    .expect("fork navigation result");
    assert!(result.workspace.is_none());
    assert!(result.snapshot.is_some());
    let request_id = controller.state.begin_session_load(fork.id);
    let target = SessionLoadRequestTarget {
        workspace_root: controller.app.workspace.root.clone(),
        workspace_cwd: controller.app.workspace.cwd.clone(),
        project_id: controller.app.workspace.project_id,
        session_id: fork.id,
    };
    controller.apply_session_loaded_message(
        request_id,
        target,
        SessionLoadReason::CreatedEditFork,
        Ok(result),
    );

    assert_eq!(controller.state.app_state.current_session_id, Some(fork.id));
    assert_eq!(controller.state.selected_session_id(), Some(fork.id));
    assert!(
        controller
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == original.id)
    );
    assert!(
        controller
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == fork.id)
    );
    assert_eq!(
        controller
            .state
            .open_session
            .as_ref()
            .map(|open| open.session_id()),
        Some(fork.id)
    );
    assert_eq!(controller.state.composer.retained_images.len(), 1);
    for session_id in [original.id, fork.id] {
        let result = load_session_navigation_result(
            controller.app.clone(),
            session_id,
            SessionLoadReason::UserSelection,
        )
        .await
        .unwrap();
        let request_id = controller.state.begin_session_load(session_id);
        controller.apply_session_loaded_message(
            request_id,
            SessionLoadRequestTarget {
                workspace_root: root.clone(),
                workspace_cwd: root.clone(),
                project_id: controller.app.workspace.project_id,
                session_id,
            },
            SessionLoadReason::UserSelection,
            Ok(result),
        );
        assert_eq!(
            controller.state.composer.retained_images.len(),
            usize::from(session_id == fork.id),
            "only the exact edit draft restores its saved image"
        );
    }
    controller.edit_image_drafts.insert(
        original.id,
        (controller.app.workspace.project_id, Vec::new()),
    );
    assert!(controller.delete_session(fork.id));
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() && !controller.state.navigation_loading()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!controller.state.background_mutation_pending());
    assert!(!controller.state.navigation_loading());
    assert!(!controller.edit_image_drafts.contains_key(&fork.id));
    assert!(controller.edit_image_drafts.contains_key(&original.id));
    assert!(controller.state.composer.retained_images.is_empty());
}

#[tokio::test]
async fn abort_permission_answer_clears_the_modal_and_waits_for_new_instructions() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let request = test_permission("abort this operation");
    let (response, receiver) = mpsc::channel();
    let run_control = RunControl::new();
    let run_control_observer = run_control.clone();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request: request.clone(),
        responder: response,
        run_control,
    });

    assert_eq!(
        controller.answer_permission(42, ReviewDecision::Abort),
        PendingPermissionResolution::Resolved
    );
    assert_eq!(receiver.try_recv(), Ok(ReviewDecision::Abort));
    assert_eq!(run_control_observer.cause(), None);
    assert!(controller.pending_permission.is_none());
    assert_eq!(
        controller.state.app_state.status_message.as_deref(),
        Some(crate::tui::state::permission_decision_pending_status_message().as_str())
    );
}

#[tokio::test]
async fn abort_permission_answer_preserves_an_existing_terminal_owner() {
    let causes = [
        RunCancellationCause::Interruption(TurnInterruptionCause::UserStop),
        RunCancellationCause::Failure("provider failed first".to_string()),
        RunCancellationCause::Superseded,
    ];

    for cause in causes {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let request = test_permission("late abort");
        let (response, receiver) = mpsc::channel();
        let run_control = RunControl::new();
        assert!(run_control.cancel(cause.clone()));
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control,
        });

        assert_eq!(
            controller.answer_permission(42, ReviewDecision::Abort),
            PendingPermissionResolution::AlreadyTerminal(cause.clone())
        );
        assert!(controller.pending_permission.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(
            controller.state.app_state.status_message,
            Some(crate::tui::state::run_cancellation_status_message(&cause))
        );
    }
}

#[tokio::test]
async fn permission_responder_failure_remains_an_operational_failure() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let request = test_permission("disconnected responder");
    let (response, receiver) = mpsc::channel();
    drop(receiver);
    let run_control = RunControl::new();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request: request.clone(),
        responder: response,
        run_control: run_control.clone(),
    });

    let outcome = controller.answer_permission(42, ReviewDecision::Approved);
    let PendingPermissionResolution::Failed(cause) = outcome else {
        panic!("disconnected responder must be a typed failure");
    };
    assert!(matches!(
        &cause,
        RunCancellationCause::Failure(message)
            if message.contains("desktop permission response failed")
    ));
    assert_eq!(run_control.cause(), Some(cause.clone()));
    assert!(controller.pending_permission.is_none());
    assert_eq!(
        controller.state.app_state.status_message,
        Some(crate::tui::state::run_cancellation_status_message(&cause))
    );
}

#[test]
fn responder_failure_after_a_competing_terminal_claim_consumes_the_ticket() {
    for decision in [ReviewDecision::Approved, ReviewDecision::Abort] {
        let request = test_permission("post-take terminal race");
        let (response, receiver) = mpsc::channel();
        drop(receiver);
        let run_control = RunControl::new();
        let cause = RunCancellationCause::Failure(format!(
            "competing failure before {decision:?} delivery"
        ));
        let mut pending = Some(PendingPermission {
            confirmation_id: 42,
            request,
            responder: response,
            run_control: run_control.clone(),
        });

        let resolution = resolve_pending_permission_after_take(&mut pending, 42, decision, |_| {
            assert!(run_control.cancel(cause.clone()));
        });

        assert_eq!(
            resolution,
            PendingPermissionResolution::AlreadyTerminal(cause.clone())
        );
        assert!(pending.is_none());
        assert_eq!(run_control.cause(), Some(cause));
    }
}

#[tokio::test]
async fn synchronous_access_change_does_not_resolve_an_inflight_permission() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let initial_access_mode = crate::config::AccessMode::Default;
    controller.app.config.permissions.access_mode = initial_access_mode;
    controller
        .state
        .provider_config
        .update_access_mode(initial_access_mode);
    let request = test_permission("closed synchronous permission responder");
    let (response, receiver) = mpsc::channel();
    drop(receiver);
    let run_control = RunControl::new();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request: request.clone(),
        responder: response,
        run_control: run_control.clone(),
    });

    assert!(controller.toggle_access_mode_with_persistence(
        |expected, access_mode| {
            assert_eq!(expected, initial_access_mode);
            assert_eq!(access_mode, crate::config::AccessMode::AutoReview);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        |_, _| Ok(()),
    ));

    assert_eq!(run_control.cause(), None);
    assert!(controller.pending_permission.is_some());
    let status = controller
        .state
        .app_state
        .status_message
        .as_deref()
        .expect("failure status");
    assert!(status.contains("次の承認判断から適用されます"));
    assert!(status.contains("表示中の承認依頼は変更しません"));
}

#[tokio::test]
async fn asynchronous_access_change_does_not_resolve_an_inflight_permission() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let initial_access_mode = crate::config::AccessMode::Default;
    controller.app.config.permissions.access_mode = initial_access_mode;
    controller
        .state
        .provider_config
        .update_access_mode(initial_access_mode);
    let request = test_permission("closed asynchronous permission responder");
    let (response, receiver) = mpsc::channel();
    drop(receiver);
    let run_control = RunControl::new();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request: request.clone(),
        responder: response,
        run_control: run_control.clone(),
    });

    assert!(controller.start_access_mode_persistence(
        move |expected, access_mode| {
            assert_eq!(expected, initial_access_mode);
            assert_eq!(access_mode, crate::config::AccessMode::AutoReview);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        |_, _| Ok(None),
    ));
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!controller.state.background_mutation_pending());
    assert_eq!(run_control.cause(), None);
    assert!(controller.pending_permission.is_some());
    let status = controller
        .state
        .app_state
        .status_message
        .as_deref()
        .expect("failure status");
    assert!(status.contains("次の承認判断から適用されます"));
    assert!(status.contains("表示中の承認依頼は変更しません"));
}

#[tokio::test]
async fn stop_routes_through_root_owner_and_waits_for_permission_cancellation_event() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let root_control = RunControl::new();
    controller.run_lifecycle.begin(1, root_control.clone());
    let request = test_permission("child stop routing");
    let (response, receiver) = mpsc::channel();
    let child_control = RunControl::new();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request: request.clone(),
        responder: response,
        run_control: child_control.clone(),
    });

    assert!(controller.cancel_root_run_at_generation(1));

    assert_eq!(
        root_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop
        ))
    );
    assert_eq!(
        child_control.cause(),
        None,
        "the Desktop Stop surface must not classify a pending child directly"
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(controller.pending_permission.is_some());

    assert!(child_control.interrupt(TurnInterruptionCause::TreeStopped));
    controller
        .runtime_tx
        .send(RuntimeMessage::PermissionCancelled {
            confirmation_id: 42,
        })
        .expect("permission cancellation event");
    controller.drain_runtime_messages();
    assert!(controller.pending_permission.is_none());
}

#[tokio::test]
async fn desktop_exit_waits_for_root_and_side_receipts_and_closes_new_admission() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let release = CancellationToken::new();
    let root_control = RunControl::new();
    controller.run_lifecycle.begin(1, root_control.clone());
    let control = root_control.clone();
    let root_release = release.clone();
    let sender = controller.control_tx.clone();
    let root_worker = controller
        .root_task_runtime
        .spawn(1, move || async move {
            control.token().cancelled().await;
            root_release.cancelled().await;
            publish_desktop_run_finished(&sender, 1, Err("stopped by Exit".to_string()));
        })
        .expect("root worker");
    assert!(
        controller
            .run_lifecycle
            .attach_worker(1, root_worker)
            .is_ok()
    );
    let owner = SessionId::new();
    let side_id = SideChatId::new();
    let side_cancel = CancellationToken::new();
    let cancel = side_cancel.clone();
    let side_release = release.clone();
    let sender = controller.control_tx.clone();
    let worker = controller
        .side_chat_task_runtime
        .spawn(1, move || async move {
            cancel.cancelled().await;
            side_release.cancelled().await;
            let _ = sender.send(RuntimeMessage::SideChatFinished {
                owner_session_id: owner,
                side_chat_id: side_id.to_string(),
                run_generation: 1,
                result: Ok(()),
            });
        })
        .expect("side worker");
    controller.side_chat_runs.insert(
        owner,
        DesktopSideChatRun {
            side_chat_id: side_id.to_string(),
            run_generation: 1,
            cancel: side_cancel.clone(),
            worker,
            phase: "running".to_string(),
            streamed_text: String::new(),
            delete_after_finish: false,
        },
    );
    let prompt_cancel = CancellationToken::new();
    controller
        .state
        .begin_prompt_enhance(1, "review", prompt_cancel.clone());

    controller.begin_exit().expect("begin Exit");
    assert!(root_control.is_cancelled());
    assert!(side_cancel.is_cancelled());
    assert!(prompt_cancel.is_cancelled());
    assert!(
        !controller
            .local_exit_settled()
            .expect("receipts still pending")
    );
    controller
        .begin_exit()
        .expect("duplicate Exit does not replace its drain");
    assert!(!controller.start_prompt_enhance("new work".to_string()));
    assert!(
        controller
            .start_side_chat(owner, side_id, 1, 0, None, None, "new work".to_string())
            .is_err()
    );
    assert!(!controller.start_run("new work".to_string()));

    release.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !controller
            .local_exit_settled()
            .expect("settle local workers")
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("local workers settled");
    assert!(controller.side_chat_runs.is_empty());
    assert!(!controller.run_lifecycle.root_is_active());
    assert!(!controller.start_run("work after settlement".to_string()));
    controller
        .begin_exit()
        .expect("Runner retry reuses settled local receipt");
}

#[tokio::test]
async fn desktop_exit_keeps_admission_closed_and_retries_a_failed_local_stop() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    controller.exit_drain = Some(DesktopExitDrain::Completed(Err(
        "exact Stop failed".to_string()
    )));
    assert_eq!(
        controller.local_exit_settled().unwrap_err(),
        "exact Stop failed"
    );
    assert!(!controller.start_run("new work".to_string()));
    assert!(!controller.start_prompt_enhance("new review".to_string()));
    controller
        .begin_exit()
        .expect("retry reacquires current stop owners");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !controller.local_exit_settled().expect("successful retry") {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("retried local stop settled");
    assert!(!controller.start_run("still closed".to_string()));
}

#[tokio::test]
async fn desktop_exit_waits_for_cancelled_prompt_transport_to_finish_its_route() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let cancellation = CancellationToken::new();
    controller
        .state
        .begin_prompt_enhance(4, "review", cancellation.clone());
    let (release, receiver) = mpsc::channel();
    controller
        .prompt_review_workers
        .push(std::thread::spawn(move || {
            // Model cancellation can finish before the Hub route's cleanup.
            receiver.recv().expect("allow route cleanup to finish");
        }));
    controller.begin_exit().expect("begin Exit");
    assert!(cancellation.is_cancelled());
    assert!(!controller.state.prompt_enhance_pending());
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        !controller
            .local_exit_settled()
            .expect("route still finishing")
    );
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !controller.local_exit_settled().expect("prompt route ended") {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("thread receipt confirmed");
}

#[tokio::test]
async fn desktop_main_stop_targets_only_the_current_root_and_preserves_child_owners() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session = controller
        .app
        .session_service
        .start_or_resume(
            crate::session::SessionStartRequest {
                selector: crate::session::SessionSelector::New,
                title: Some("desktop tree Stop".to_string()),
                cwd: root,
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: controller.app.config.permissions.access_mode,
                provider_connection: Some(
                    crate::session::SessionProviderConnection::from_model_config(
                        &controller.app.config.model,
                    ),
                ),
            },
            controller.app.workspace.clone(),
        )
        .await
        .expect("root session");
    let root_scope = RunControl::new();
    let runtime = controller.app.process_runtime.agent_runtime();
    let root_execution = runtime
        .begin_root(
            &session,
            Arc::new(
                crate::config::ResolvedTurnConfig::capture(controller.app.config.clone())
                    .expect("valid test turn config"),
            ),
            SharedConfirmationPrompt::new(DesktopConfirmationPrompt {
                control: controller.control_tx.clone(),
                next_permission_request_id: controller.next_permission_request_id.clone(),
            }),
            root_scope.clone(),
        )
        .await
        .expect("root execution");
    let root_turn_id = TurnId::new();
    let root_admission_guard = root_scope
        .begin_root_admission(session.session.id, root_turn_id)
        .expect("publish root admission plan");
    let root_admission = controller
        .app
        .store
        .session_repo()
        .admit_session_turn(session.session.id, root_turn_id)
        .await
        .expect("root admission")
        .expect("root admitted");
    root_admission_guard
        .commit(root_admission.admission_revision)
        .expect("publish canonical root admission receipt");
    root_execution
        .context
        .bind_durable_turn_owner(
            root_admission.admission_id,
            root_turn_id,
            root_admission.admission_revision,
        )
        .expect("bind root owner");
    let tree_control = root_execution.context.tree_control_for_test();
    let (_, child_a) = tree_control
        .register_child(
            &crate::runtime::AgentPath::root(),
            "child_a",
            SessionId::new(),
            Some("already interrupted".to_string()),
        )
        .expect("child A");
    let (_, child_b) = tree_control
        .register_child(
            &crate::runtime::AgentPath::root(),
            "child_b",
            SessionId::new(),
            Some("held sibling".to_string()),
        )
        .expect("child B");
    let child_a_control = child_a.run_control();
    let child_b_control = child_b.run_control();
    tree_control
        .cancel_agent(&crate::runtime::AgentPath::try_from("/root/child_a").expect("path"))
        .expect("exact child A interrupt");

    controller.state.app_state.current_session_id = Some(session.session.id);
    controller.run_lifecycle.begin(44, root_scope.clone());
    controller.state.begin_agent_run();
    assert!(controller.cancel_root_run_at_generation(44));

    for _ in 0..200 {
        controller.drain_runtime_messages();
        if !controller.session_projection_refresh_requests.is_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        !controller.session_projection_refresh_requests.is_pending(),
        "durable root Stop refresh must settle"
    );
    assert!(
        !tree_control.tree_is_cancelled(),
        "ordinary root Stop must not acquire the explicit tree-wide cancellation owner"
    );
    assert_eq!(
        root_execution.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop,
        ))
    );
    assert_eq!(
        child_a_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::AgentInterrupted,
        )),
        "Main Stop must not overwrite the prior exact child terminal owner"
    );
    assert_eq!(
        child_b_control.cause(),
        None,
        "ordinary root Stop must not classify an unaffected sibling"
    );
    let repository = controller.app.store.session_repo();
    assert!(
        repository
            .durable_terminal_for_turn(session.session.id, root_turn_id)
            .await
            .expect("pre-settlement root terminal lookup")
            .is_none(),
        "Stop records intent; the admitted owner remains the terminal writer"
    );
    let renewal = repository
        .renew_admitted_run_lease(
            session.session.id,
            root_admission.admission_id,
            root_turn_id,
        )
        .await
        .expect("root owner observes Stop intent");
    assert!(
        matches!(
            renewal,
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::InterruptRequested(
                TurnInterruptionCause::UserStop
            )
        ),
        "unexpected root lease renewal after Stop: {renewal:?}"
    );
    let settlement = repository
        .settle_admitted_turn_with_protocol_event(
            session.session.id,
            root_admission.admission_id,
            &RunEvent::TurnTerminal {
                session_id: session.session.id,
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                }),
            },
            root_turn_id,
            None,
            None,
        )
        .await
        .expect("admitted root terminal settlement");
    assert_eq!(
        settlement.commit(),
        crate::storage::session_repo::AdmittedTerminalCommit::Applied
    );
    let settled_terminal = settlement
        .into_terminal()
        .expect("authoritative root terminal receipt");
    assert_eq!(
        settled_terminal.outcome,
        crate::protocol::TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::UserStop,
        },
        "the committed Stop request overrides a later success proposal"
    );

    tree_control
        .complete_execution(
            child_a,
            crate::runtime::InactiveAgentStatus::Interrupted,
            None,
        )
        .expect("settle child A");
    tree_control
        .complete_execution(
            child_b,
            crate::runtime::InactiveAgentStatus::Completed(None),
            None,
        )
        .expect("settle child B");
    runtime.complete_root(
        root_execution,
        &Ok(RunSummary::from_terminal(
            session.session.id,
            root_turn_id,
            settled_terminal,
        )),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop,
        )),
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.wait_for_tree_quiescence(session.session.id),
    )
    .await
    .expect("tree quiescence timeout")
    .expect("tree quiescence");
}

#[tokio::test]
async fn permission_without_an_exact_execution_owner_has_no_stop_capability() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let request = test_permission("detached child approval");
    let (response, receiver) = mpsc::channel();
    let run_control = RunControl::new();
    let observer = run_control.clone();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request,
        responder: response,
        run_control,
    });

    let projection = controller.next_web_state().expect("permission projection");
    assert!(!projection.can_cancel_run);
    assert!(projection.stop_target.is_none());
    assert!(projection.confirmation_visible);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        observer.cause(),
        None,
        "permission presence cannot mint an implicit UserStop owner"
    );
    assert!(controller.pending_permission.is_some());
}

#[tokio::test]
async fn terminal_permission_projection_preserves_the_existing_first_writer() {
    for cause in [
        RunCancellationCause::Interruption(TurnInterruptionCause::ApprovalAborted),
        RunCancellationCause::Failure("provider failed first".to_string()),
    ] {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let (response, receiver) = mpsc::channel();
        let run_control = RunControl::new();
        assert!(run_control.cancel(cause.clone()));
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: test_permission("late Stop"),
            responder: response,
            run_control: run_control.clone(),
        });

        let projection = controller.next_web_state().expect("terminal projection");
        assert_eq!(run_control.cause(), Some(cause.clone()));
        assert!(!projection.can_cancel_run);
        assert!(projection.stop_target.is_none());
        assert!(!projection.confirmation_visible);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}

#[tokio::test]
async fn ordinary_stop_does_not_target_a_detached_child_after_root_completion() {
    use crate::session::{NewSession, SessionRepository as _};

    let (_temp, workspace_root, mut controller) = empty_access_test_controller().await;
    let repository = controller.app.store.session_repo();
    let new_session = |title: &str| NewSession {
        project_id: controller.app.workspace.project_id,
        title: title.to_string(),
        cwd: workspace_root.clone(),
        model: controller.app.config.model.model.clone(),
        base_url: controller.app.config.model.base_url.clone(),
        access_mode: crate::config::AccessMode::Default,
        provider_connection: None,
    };
    let root = repository
        .create_session(new_session("durable root"))
        .await
        .expect("root session");
    let child = repository
        .create_session(new_session("durable child"))
        .await
        .expect("child session");
    repository
        .insert_session_spawn_edge(
            root.id,
            root.id,
            child.id,
            "/root/durable_child",
            "durable_child",
        )
        .await
        .expect("spawn edge");
    let root_turn = crate::protocol::TurnId::new();
    repository
        .admit_session_turn(root.id, root_turn)
        .await
        .expect("root admission")
        .expect("root admitted");
    controller
        .app
        .store
        .protocol_event_store()
        .seed_history_item_for_test(&crate::protocol::HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: root.id,
            scope: crate::protocol::HistoryScope::Turn { turn_id: root_turn },
            sequence_no: 1,
            created_at_ms: 1,
            payload: crate::protocol::HistoryItemPayload::UserTurn {
                content: vec![crate::protocol::ContentPart::Text {
                    text: "run detached work".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        })
        .expect("root history");
    let root_target = repository
        .captured_running_terminal_target(root.id)
        .await
        .expect("capture root target")
        .expect("root running target");
    assert!(
        repository
            .terminalize_captured_running_session_with_protocol_event(
                root.id,
                &RunEvent::TurnTerminal {
                    session_id: root.id,
                    terminal: Box::new(crate::session::DurableTurnTerminal {
                        outcome: crate::protocol::TurnTerminalOutcome::Completed,
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    }),
                },
                root_target,
            )
            .await
            .expect("complete root")
    );
    repository
        .admit_session_turn(child.id, crate::protocol::TurnId::new())
        .await
        .expect("child admission")
        .expect("child admitted");
    controller.state.app_state.current_session_id = Some(root.id);
    controller.state.app_state.run_status = crate::tui::state::RunStatus::Completed;
    controller.loaded_agent_activity_records = Some((
        root.id,
        vec![agent_record(
            child.id,
            "/root/durable_child",
            AgentStatus::Running,
            "",
        )],
    ));
    assert!(controller.current_agent_tree_active());
    assert!(!controller.run_lifecycle.root_is_active());

    let projection = controller.next_web_state().expect("child-only projection");

    assert!(!controller.session_projection_refresh_requests.is_pending());
    assert!(!projection.can_cancel_run);
    assert!(projection.stop_target.is_none());
    assert_eq!(
        repository
            .get_session(root.id)
            .await
            .expect("preserved root")
            .status,
        SessionStatus::Completed
    );
    assert_eq!(
        repository
            .get_session(child.id)
            .await
            .expect("detached child")
            .status,
        SessionStatus::Running
    );
    controller.loaded_agent_activity_records = Some((
        root.id,
        vec![agent_record(
            child.id,
            "/root/stale-child",
            AgentStatus::Running,
            "",
        )],
    ));
    let stale_projection = controller
        .next_web_state()
        .expect("stale child-only projection");
    assert_eq!(
        controller.state.app_state.run_status,
        crate::tui::state::RunStatus::Completed,
        "a child-only Stop must not reclassify the preserved root"
    );
    assert!(!stale_projection.can_cancel_run);
    assert!(stale_projection.stop_target.is_none());
}

#[tokio::test]
async fn pending_access_adoption_without_session_started_settles_global_only_on_finished() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let initial_access_mode = crate::config::AccessMode::Default;
    let expected_access_mode = initial_access_mode.next();
    controller.app.config.permissions.access_mode = initial_access_mode;
    controller
        .state
        .provider_config
        .update_access_mode(initial_access_mode);
    controller.run_lifecycle.begin(1, RunControl::new());
    let session_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    assert!(controller.start_access_mode_persistence(
        move |expected, access_mode| {
            assert_eq!(expected, initial_access_mode);
            assert_eq!(access_mode, expected_access_mode);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        {
            let session_calls = session_calls.clone();
            move |_, _| {
                session_calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        },
    ));
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if controller.pending_access_mode_adoption.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(controller.pending_access_mode_adoption.is_some());
    assert!(controller.state.background_mutation_pending());

    controller
        .runtime_tx
        .send(RuntimeMessage::Finished {
            run_generation: 1,
            result: Err("failed before session admission".to_string()),
        })
        .expect("pre-admission worker finish");
    controller.drain_runtime_messages();

    assert!(controller.pending_access_mode_adoption.is_none());
    assert!(!controller.state.background_mutation_pending());
    assert_eq!(controller.state.app_state.current_session_id, None);
    assert_eq!(session_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        controller.app.config.permissions.access_mode,
        expected_access_mode
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        expected_access_mode
    );
}

#[tokio::test]
async fn delayed_adopted_access_completion_is_discarded_after_next_root_generation_starts() {
    use crate::session::{NewSession, SessionRepository as _};

    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let reloaded_access_mode = controller.app.config.permissions.access_mode;
    let initial_access_mode = [
        crate::config::AccessMode::Default,
        crate::config::AccessMode::FullAccess,
    ]
    .into_iter()
    .find(|access_mode| access_mode.next() != reloaded_access_mode)
    .expect("one transition differs from the reloaded access owner");
    let expected_access_mode = initial_access_mode.next();
    assert_ne!(expected_access_mode, reloaded_access_mode);
    controller.app.config.permissions.access_mode = initial_access_mode;
    controller
        .state
        .provider_config
        .update_access_mode(initial_access_mode);
    let session = controller
        .app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: controller.app.workspace.project_id,
            title: "delayed adopted root".to_string(),
            cwd: root,
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: initial_access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    let session_id = session.id;
    let session_title = session.title.clone();
    controller.run_lifecycle.begin(1, RunControl::new());
    let (adopted_started_tx, adopted_started_rx) = mpsc::sync_channel(1);
    let (release_adopted_tx, release_adopted_rx) = mpsc::sync_channel(1);

    assert!(controller.start_access_mode_persistence(
        move |expected, access_mode| {
            assert_eq!(expected, initial_access_mode);
            assert_eq!(access_mode, expected_access_mode);
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        move |persisted_session_id, access_mode| {
            assert_eq!(persisted_session_id, session_id);
            assert_eq!(access_mode, expected_access_mode);
            adopted_started_tx.send(()).expect("adopted worker started");
            release_adopted_rx.recv().expect("release adopted worker");
            Ok(None)
        },
    ));
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if controller.pending_access_mode_adoption.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let old_target = controller
        .pending_access_mode_adoption
        .as_ref()
        .expect("pending adopted owner")
        .target
        .clone();
    controller
        .runtime_tx
        .send(RuntimeMessage::RunEvent {
            run_generation: 1,
            event: RunEvent::SessionStarted {
                session_id,
                title: session_title,
            },
        })
        .expect("session adoption event");
    controller.drain_runtime_messages();
    adopted_started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("adopted worker dispatch");
    assert!(controller.pending_access_mode_adoption.is_none());
    assert!(controller.state.background_mutation_pending());

    controller
        .runtime_tx
        .send(RuntimeMessage::Finished {
            run_generation: 1,
            result: Err("first root finished".to_string()),
        })
        .expect("first root finish");
    controller.drain_runtime_messages();
    assert!(!controller.run_lifecycle.root_is_active());

    controller.run_lifecycle.begin(2, RunControl::new());
    controller.next_root_run_generation = 3;
    assert_eq!(
        controller.access_mode_persistence_target_relation(&old_target),
        AccessModePersistenceTargetRelation::Stale
    );

    release_adopted_tx.send(()).expect("release adopted worker");
    for _ in 0..200 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!controller.state.background_mutation_pending());
    assert!(!controller.access_mode_persistence_requests.is_pending());
    assert_eq!(controller.run_lifecycle.root_generation(), Some(2));
    let current_access_mode = controller.app.config.permissions.access_mode;
    assert_eq!(current_access_mode, reloaded_access_mode);
    assert_ne!(current_access_mode, expected_access_mode);
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        current_access_mode
    );
}

#[tokio::test]
async fn desktop_reopen_uses_durable_session_access_for_run_config_and_new_chat_uses_global() {
    use crate::session::{NewSession, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let mut app = build_test_app(&root, store).await;
    let global_access_mode = crate::config::AccessMode::Default;
    let session_access_mode = crate::config::AccessMode::FullAccess;
    app.config.permissions.access_mode = global_access_mode;
    let session = app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: app.workspace.project_id,
            title: "durable access".to_string(),
            cwd: root.clone(),
            model: app.config.model.model.clone(),
            base_url: app.config.model.base_url.clone(),
            access_mode: session_access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    let turn_id = crate::protocol::TurnId::new();
    app.store
        .protocol_event_store()
        .seed_history_item_for_test(&crate::protocol::HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            scope: crate::protocol::HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 1,
            payload: crate::protocol::HistoryItemPayload::UserTurn {
                content: vec![crate::protocol::ContentPart::Text {
                    text: "reopen".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        })
        .expect("history item");
    let args = DesktopArgs {
        directory: Some(root),
        session_id: Some(session.id),
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");

    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        session_access_mode
    );
    let run_config = controller.state.provider_config.effective_config.clone();
    assert_eq!(run_config.permissions.access_mode, session_access_mode);

    controller.start_new_chat_with_global_access();
    assert_eq!(controller.state.app_state.current_session_id, None);
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        global_access_mode
    );
}

#[tokio::test]
async fn archiving_the_only_current_session_restores_global_access_for_the_new_chat() {
    use crate::session::{NewSession, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let mut app = build_test_app(&root, store).await;
    let global_access_mode = crate::config::AccessMode::Default;
    let session_access_mode = crate::config::AccessMode::FullAccess;
    app.config.permissions.access_mode = global_access_mode;
    let session = app
        .store
        .session_repo()
        .create_session(NewSession {
            project_id: app.workspace.project_id,
            title: "only current session".to_string(),
            cwd: root.clone(),
            model: app.config.model.model.clone(),
            base_url: app.config.model.base_url.clone(),
            access_mode: session_access_mode,
            provider_connection: None,
        })
        .await
        .expect("session");
    app.store
        .protocol_event_store()
        .seed_history_item_for_test(&crate::protocol::HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            scope: crate::protocol::HistoryScope::Turn {
                turn_id: crate::protocol::TurnId::new(),
            },
            sequence_no: 1,
            created_at_ms: 1,
            payload: crate::protocol::HistoryItemPayload::UserTurn {
                content: vec![crate::protocol::ContentPart::Text {
                    text: "archive this session".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        })
        .expect("history item");
    let args = DesktopArgs {
        directory: Some(root),
        session_id: Some(session.id),
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        session_access_mode
    );

    assert!(controller.archive_session(session.id, true));
    controller.run_lifecycle.begin(41, RunControl::new());
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!controller.state.background_mutation_pending());
    assert_eq!(controller.state.app_state.current_session_id, None);
    assert_eq!(controller.state.selected_session_id(), None);
    assert_eq!(
        desktop_web_state(&controller.state, &DesktopRuntimeProjection::default()).status_message,
        "チャットをアーカイブしました。"
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        global_access_mode
    );
    let run_config = controller.state.provider_config.effective_config.clone();
    assert_eq!(run_config.permissions.access_mode, global_access_mode);
}

#[tokio::test]
async fn blocked_access_persistence_does_not_block_stop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let args = DesktopArgs {
        directory: Some(root),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let cancel = RunControl::new();
    let cancel_observer = cancel.clone();
    controller.run_lifecycle.begin(1, cancel);
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);

    assert!(controller.start_access_mode_persistence(
        move |_, _| {
            started_tx.send(()).expect("signal blocked persistence");
            release_rx.recv().expect("release blocked persistence");
            Err("simulated blocked global writer".to_string())
        },
        |_, _| Ok(None),
    ));
    started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("persistence worker started");

    assert!(controller.cancel_root_run_at_generation(1));
    assert!(
        cancel_observer.is_cancelled(),
        "Stop must cancel the root before blocked persistence completes"
    );
    assert!(controller.state.background_mutation_pending());

    release_tx.send(()).expect("release persistence");
    for _ in 0..100 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!controller.state.background_mutation_pending());
}

#[tokio::test]
async fn blocked_access_persistence_rejects_submit_review_and_steer_admission() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let args = DesktopArgs {
        directory: Some(root),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);

    assert!(controller.start_access_mode_persistence(
        move |_, _| {
            started_tx.send(()).expect("signal blocked persistence");
            release_rx.recv().expect("release blocked persistence");
            Ok(Some(Utf8PathBuf::from("C:/config.toml")))
        },
        |_, _| Ok(None),
    ));
    started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("persistence worker started");
    assert!(controller.state.background_mutation_pending());

    let initial_generation = controller.next_root_run_generation;
    controller.state.composer.draft_prompt = "submit after access settles".to_string();
    assert!(!controller.start_run("submit after access settles".to_string()));
    assert!(!controller.start_review_uncommitted("review after access settles".to_string()));
    controller
        .state
        .begin_prompt_enhance(11, "enhance before review", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(11, "enhanced review draft".to_string())
    );
    assert!(!controller.send_prompt_review(11, true, "edited review draft".to_string()));
    assert_eq!(
        controller.state.composer.draft_prompt,
        "submit after access settles"
    );
    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| review.current_draft_text.as_str()),
        Some("edited review draft")
    );
    assert_eq!(controller.next_root_run_generation, initial_generation);
    assert!(!controller.run_lifecycle.root_is_active());
    assert!(
        controller.state.cancel_prompt_review_if_current(11),
        "the failed exact review send retains its draft until the user cancels that owner"
    );

    controller.run_lifecycle.begin(77, RunControl::new());
    assert!(!controller.start_run("steer after access settles".to_string()));
    assert_eq!(controller.run_lifecycle.root_generation(), Some(77));
    assert!(
        controller
            .state
            .app_state
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains("owner mutation"))
    );
    controller.run_lifecycle.finish_root();

    release_tx.send(()).expect("release persistence");
    for _ in 0..100 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!controller.state.background_mutation_pending());
}

#[test]
fn durable_activity_retry_is_bounded_and_config_generation_never_rewinds() {
    assert!(durable_agent_activity_retry_allowed(0));
    assert!(durable_agent_activity_retry_allowed(2));
    assert!(!durable_agent_activity_retry_allowed(3));
    assert!(!durable_agent_activity_retry_allowed(u8::MAX));

    assert_eq!(next_config_generation(1), 2);
    assert_eq!(next_config_generation(u64::MAX), u64::MAX);
}

#[test]
fn failed_steer_preserves_request_draft_and_attachments() {
    let mut state = DesktopState::new(
        super::super::models::DesktopSnapshot {
            workspace_path: "C:/workspace".to_string(),
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
    state.composer.draft_prompt = "keep this draft".to_string();
    let image = Utf8PathBuf::from("C:/workspace/reference.png");
    state.composer.image_attachment_paths.push(image.clone());

    assert!(!finish_steer_submission(
        &mut state,
        std::slice::from_ref(&image),
        Err("terminal session".to_string()),
    ));
    assert_eq!(state.composer.draft_prompt, "keep this draft");
    assert_eq!(state.composer.image_attachment_paths, vec![image]);
    assert!(
        state
            .app_state
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains("terminal session"))
    );
}

#[test]
fn accepted_steer_does_not_append_a_phantom_transcript_row() {
    let mut state = DesktopState::new(
        super::super::models::DesktopSnapshot {
            workspace_path: "C:/workspace".to_string(),
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
    let image = Utf8PathBuf::from("C:/workspace/reference.png");
    state.composer.image_attachment_paths.push(image.clone());

    assert!(finish_steer_submission(
        &mut state,
        std::slice::from_ref(&image),
        Ok(()),
    ));
    assert!(state.app_state.transcript_entries.is_empty());
    assert!(state.composer.image_attachment_paths.is_empty());
    assert!(
        state
            .app_state
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains("送信待ちキュー"))
    );
}

fn agent_record(
    session_id: SessionId,
    agent_path: &str,
    status: AgentStatus,
    result_preview: &str,
) -> AgentActivityRecord {
    AgentActivityRecord {
        agent_path: agent_path.to_string(),
        session_id,
        task_name: agent_path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string(),
        task_preview: format!("task for {agent_path}"),
        status,
        current_activity: String::new(),
        result_preview: result_preview.to_string(),
        started_order: 1,
        updated: false,
        is_current_turn: false,
        active_turn_id: None,
        interrupt_target: None,
    }
}

fn test_permission(summary: &str) -> PermissionRequest {
    PermissionRequest {
        access: crate::workspace::AccessKind::Shell,
        summary: summary.to_string(),
        details: Vec::new(),
        targets: vec![Utf8PathBuf::from("C:/workspace")],
        outside_workspace: false,
        risks: Vec::new(),
        agent_path: Some(format!("/root/{summary}")),
        agent_task_name: Some(summary.to_string()),
    }
}

fn recv_runtime_message(receiver: &mpsc::Receiver<RuntimeMessage>) -> RuntimeMessage {
    for _ in 0..200 {
        match receiver.try_recv() {
            Ok(message) => return message,
            Err(mpsc::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("desktop runtime channel disconnected")
            }
        }
    }
    panic!("timed out waiting for desktop runtime message")
}

#[test]
fn child_only_agent_activity_does_not_block_desktop_navigation() {
    assert_eq!(
        navigation_admission_blocker(false, false, false, false),
        None
    );
    assert_eq!(
        navigation_admission_blocker(false, false, false, true),
        Some("the current run is finalizing")
    );
}

#[tokio::test]
async fn child_only_agent_activity_keeps_in_flight_session_search_owned() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let root_session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(root_session_id);
    controller.loaded_agent_activity_records = Some((
        root_session_id,
        vec![agent_record(
            SessionId::new(),
            "/root/detached_search_worker",
            AgentStatus::Running,
            "",
        )],
    ));
    let operation_id = controller.state.begin_session_search();
    let request_id = controller
        .session_search_requests
        .begin(
            operation_id,
            SessionSearchRequestTarget {
                query: "retained query".to_string(),
                include_archived: false,
                selected_session_id: Some(root_session_id),
            },
        )
        .dispatch
        .expect("initial search dispatch")
        .request_id;

    let projection = controller.next_web_state().expect("child-only projection");

    assert!(projection.agent_tree_active);
    assert!(projection.navigation_admission_open);
    assert!(projection.can_submit);
    assert!(
        controller
            .state
            .pending_async_operation_keys()
            .iter()
            .any(|key| key == "session_search"),
        "descendant liveness must not cancel the root-owned search"
    );
    assert!(
        controller
            .session_search_requests
            .finish(request_id)
            .is_some(),
        "the accepted search request must retain its completion owner"
    );
}

#[test]
fn session_search_started_while_idle_cannot_replace_root_after_root_admission() {
    fn snapshot(session_id: SessionId, title: &str) -> DesktopSnapshot {
        DesktopSnapshot {
            workspace_path: "C:/workspace".to_string(),
            provider_label: String::new(),
            model_label: String::new(),
            command_rows: Vec::new(),
            project_rows: Vec::new(),
            selected_project_index: 0,
            session_rows: vec![super::super::models::DesktopSessionRow::from_parts(
                session_id,
                title,
                SessionStatus::Idle,
            )],
            chat_session_rows: Vec::new(),
            session_details: Vec::new(),
            selected_session_index: 0,
        }
    }

    let selected_root = SessionId::new();
    let stale_search_root = SessionId::new();
    let mut state = DesktopState::new(
        snapshot(selected_root, "selected root"),
        ResolvedConfig::default(),
    );

    assert!(!apply_session_search_result(
        &mut state,
        true,
        true,
        Ok(snapshot(stale_search_root, "stale search result")),
    ));
    assert_eq!(state.selected_session_id(), Some(selected_root));

    assert!(apply_session_search_result(
        &mut state,
        true,
        false,
        Ok(snapshot(stale_search_root, "child-safe search result")),
    ));
    assert_eq!(state.selected_session_id(), Some(stale_search_root));
    assert!(!session_search_result_can_apply(false, false));
}

fn navigation_owner_state() -> (DesktopState, SessionId, SessionId) {
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let mut state = DesktopState::new(
        DesktopSnapshot {
            workspace_path: "C:/workspace-a".to_string(),
            provider_label: String::new(),
            model_label: String::new(),
            command_rows: Vec::new(),
            project_rows: vec![
                super::super::models::DesktopProjectRow {
                    project_id: ProjectId::new(),
                    label: "project A".to_string(),
                    path: "C:/workspace-a".to_string(),
                },
                super::super::models::DesktopProjectRow {
                    project_id: ProjectId::new(),
                    label: "project B".to_string(),
                    path: "C:/workspace-b".to_string(),
                },
            ],
            selected_project_index: 0,
            session_rows: vec![
                super::super::models::DesktopSessionRow::from_parts(
                    session_a,
                    "session A",
                    SessionStatus::Idle,
                ),
                super::super::models::DesktopSessionRow::from_parts(
                    session_b,
                    "session B",
                    SessionStatus::Idle,
                ),
            ],
            chat_session_rows: Vec::new(),
            session_details: Vec::new(),
            selected_session_index: 0,
        },
        ResolvedConfig::default(),
    );
    state.app_state.current_session_id = Some(session_a);
    state.rebind_composer_owner(Some(session_a));
    state.composer.draft_prompt = "draft owned by A".to_string();
    (state, session_a, session_b)
}

#[test]
fn failed_session_navigation_restores_selected_and_draft_owner_to_a() {
    let (mut state, session_a, session_b) = navigation_owner_state();
    let attachment = Utf8PathBuf::from("C:/workspace-a/attachment.png");
    state
        .composer
        .image_attachment_paths
        .push(attachment.clone());
    state.select_session(1);
    assert_eq!(state.selected_session_id(), Some(session_b));
    let request_id = state.begin_session_load(session_b);

    assert!(finish_navigation_failure(
        &mut state,
        request_id,
        "session B failed to load",
    ));

    let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
    assert_eq!(state.selected_session_id(), Some(session_a));
    assert!(projection.selected_session_title.starts_with("session A"));
    assert_eq!(
        projection.draft_target.session_id,
        Some(session_a.to_string())
    );
    assert_eq!(projection.draft_prompt, "draft owned by A");
    assert_eq!(state.composer.image_attachment_paths, vec![attachment]);
    assert!(!projection.navigation_loading);
}

#[test]
fn failed_project_navigation_never_replaces_committed_workspace_owner() {
    let (mut state, session_a, _) = navigation_owner_state();
    let request_id = state.begin_workspace_load(Utf8PathBuf::from("C:/workspace-b"), None);

    assert_eq!(state.snapshot.workspace_path, "C:/workspace-a");
    assert_eq!(state.selected_project_path(), Some("C:/workspace-a"));
    assert!(finish_navigation_failure(
        &mut state,
        request_id,
        "project B failed to load",
    ));

    let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
    assert_eq!(projection.workspace_path, "C:/workspace-a");
    assert_eq!(state.selected_project_path(), Some("C:/workspace-a"));
    assert_eq!(state.selected_session_id(), Some(session_a));
    assert_eq!(
        projection.draft_target.session_id,
        Some(session_a.to_string())
    );
    assert_eq!(projection.draft_prompt, "draft owned by A");
    assert!(!projection.navigation_loading);
}

#[test]
fn non_selected_row_mutations_preserve_owner_a_on_success_and_failure() {
    let (mut state, session_a, session_b) = navigation_owner_state();
    let project_a = state.snapshot.project_rows[0].project_id;
    let project_b = state.snapshot.project_rows[1].project_id;

    let mut session_success = state.snapshot.clone();
    session_success
        .session_rows
        .iter_mut()
        .find(|row| row.session_id == session_b)
        .expect("session B")
        .archived = true;
    session_success
        .session_rows
        .retain(|row| row.session_id != session_a);
    let archive_id = state.begin_session_archive_mutation();
    assert!(state.finish_session_archive_mutation(archive_id));
    state.replace_snapshot_preserving_current_owner(session_success);
    assert_eq!(state.selected_session_id(), Some(session_a));
    assert_eq!(state.app_state.current_session_id, Some(session_a));

    let maintenance_id = state.begin_session_maintenance_mutation();
    assert!(state.finish_session_maintenance_mutation(maintenance_id));
    state.set_status_message("session B mutation failed");
    assert_eq!(state.selected_session_id(), Some(session_a));
    assert_eq!(state.app_state.current_session_id, Some(session_a));

    let mut project_success = state.snapshot.clone();
    project_success
        .project_rows
        .retain(|row| row.project_id != project_b);
    let delete_id = state.begin_project_delete_mutation();
    assert!(state.finish_project_delete_mutation(delete_id));
    state.replace_snapshot(project_success);
    assert_eq!(state.selected_project_id(), Some(project_a));
    assert_eq!(state.selected_session_id(), Some(session_a));
    assert_eq!(state.app_state.current_session_id, Some(session_a));

    let failed_delete_id = state.begin_project_delete_mutation();
    assert!(state.finish_project_delete_mutation(failed_delete_id));
    state.set_status_message("project B deletion failed");
    let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
    assert_eq!(state.selected_project_id(), Some(project_a));
    assert_eq!(state.selected_session_id(), Some(session_a));
    assert_eq!(
        projection.draft_target.session_id,
        Some(session_a.to_string())
    );
    assert_eq!(projection.workspace_path, "C:/workspace-a");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controller_async_owner_and_durable_submission_contracts_are_lossless() {
    use crate::session::{ProjectRepository as _, SessionRepository as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let project_b_root =
        Utf8PathBuf::from_path_buf(temp.path().join("workspace-b")).expect("utf8 project B");
    std::fs::create_dir_all(&project_b_root).expect("project B workspace");
    let project_b = ProjectId::new();
    app.store
        .project_repo()
        .upsert_project(project_b, &project_b_root, "project B", "none")
        .await
        .expect("project B");
    let args = DesktopArgs {
        directory: Some(root.clone()),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    let repo = controller.app.store.session_repo();
    let create = |title: &str| crate::session::NewSession {
        project_id: controller.app.workspace.project_id,
        title: title.to_string(),
        cwd: root.clone(),
        model: controller.app.config.model.model.clone(),
        base_url: controller.app.config.model.base_url.clone(),
        access_mode: controller.app.config.permissions.access_mode,
        provider_connection: None,
    };
    let session_a = repo.create_session(create("session A")).await.expect("A");
    let session_b = repo.create_session(create("session B")).await.expect("B");
    let snapshot = load_snapshot_for_selection(&controller.app, Some(session_a.id))
        .await
        .expect("snapshot");
    controller.state.replace_snapshot(snapshot);
    controller.state.app_state.current_session_id = Some(session_a.id);
    controller.state.app_state.current_session_title = session_a.title.clone();
    let stale_snapshot = controller.state.snapshot.clone();

    let search_operation = controller.state.begin_session_search();
    let search_request = controller
        .session_search_requests
        .begin(
            search_operation,
            SessionSearchRequestTarget {
                query: "session".to_string(),
                include_archived: false,
                selected_session_id: Some(session_a.id),
            },
        )
        .dispatch
        .expect("first search dispatch")
        .request_id;
    let snapshot_target = SnapshotRequestTarget {
        workspace_root: root.clone(),
        selected_session_id: Some(session_a.id),
    };
    let snapshot_request = controller.snapshot_requests.begin(snapshot_target.clone());
    controller.state.begin_snapshot_refresh();

    assert!(controller.archive_session(session_b.id, true));
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!controller.state.background_mutation_pending());
    assert_eq!(
        controller.state.app_state.current_session_id,
        Some(session_a.id)
    );
    assert_eq!(controller.state.selected_session_id(), Some(session_a.id));
    assert!(
        !controller
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_b.id)
    );

    controller
        .runtime_tx
        .send(RuntimeMessage::SessionSearchLoaded {
            request_id: search_request,
            result: Ok(stale_snapshot.clone()),
        })
        .expect("stale search");
    controller
        .runtime_tx
        .send(RuntimeMessage::SnapshotLoaded {
            request_id: snapshot_request,
            target: snapshot_target,
            result: Ok(stale_snapshot),
        })
        .expect("stale snapshot");
    controller.drain_runtime_messages();

    assert_eq!(
        controller.state.app_state.current_session_id,
        Some(session_a.id)
    );
    assert_eq!(controller.state.selected_session_id(), Some(session_a.id));
    assert!(
        !controller
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_b.id)
    );
    assert!(
        !controller
            .state
            .pending_async_operation_keys()
            .iter()
            .any(|key| key == "session_search" || key == "snapshot_refresh")
    );

    assert!(
        controller
            .state
            .snapshot
            .project_rows
            .iter()
            .any(|row| row.project_id == project_b)
    );
    let project_b_draft = SessionId::new();
    controller
        .edit_image_drafts
        .insert(project_b_draft, (project_b, Vec::new()));
    controller.edit_image_drafts.insert(
        session_a.id,
        (controller.app.workspace.project_id, Vec::new()),
    );
    assert!(controller.delete_project(project_b));
    controller.app.config.model.model = "live-config-after-delete-dispatch".to_string();
    for _ in 0..300 {
        controller.drain_runtime_messages();
        if !controller.state.background_mutation_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!controller.state.background_mutation_pending());
    assert!(!controller.state.navigation_loading());
    assert!(!controller.edit_image_drafts.contains_key(&project_b_draft));
    assert!(controller.edit_image_drafts.contains_key(&session_a.id));
    assert_eq!(controller.app.workspace.root, root);
    assert_eq!(
        controller.app.config.model.model,
        "live-config-after-delete-dispatch"
    );
    assert_eq!(
        controller.state.app_state.current_session_id,
        Some(session_a.id)
    );
    assert_eq!(controller.state.selected_session_id(), Some(session_a.id));
    assert!(
        !controller
            .state
            .snapshot
            .project_rows
            .iter()
            .any(|row| row.project_id == project_b)
    );
    assert_eq!(
        desktop_web_state(&controller.state, &DesktopRuntimeProjection::default()).status_message,
        "プロジェクトを削除しました。"
    );

    let retained_image = root.join("retained.png");
    controller
        .state
        .composer
        .image_attachment_paths
        .push(retained_image.clone());
    controller
        .state
        .begin_prompt_enhance(77, "raw review", CancellationToken::new());
    assert!(
        controller
            .state
            .finish_prompt_enhance(77, "edited review".to_string())
    );
    let review_target = controller
        .prompt_review_target(77)
        .expect("review target before run admission");
    let failed_generation = 900;
    controller
        .run_lifecycle
        .begin(failed_generation, RunControl::new());
    controller.state.begin_agent_run();
    controller.pending_root_submission = Some(PendingRootSubmission {
        run_generation: failed_generation,
        owner_workspace_path: root.clone(),
        owner_session_id: Some(session_a.id),
        prompt_dispatch: crate::session::PromptDispatchPart::raw("retain on preflight error"),
        image_paths: vec![retained_image.clone()],
        prompt_review_to_cancel: Some(review_target.clone()),
    });
    assert!(
        controller
            .state
            .selected_detail()
            .transcript_rows
            .iter()
            .all(|row| row.row_kind != super::super::models::DesktopTranscriptRowKind::User)
    );
    controller
        .runtime_tx
        .send(RuntimeMessage::RunEvent {
            run_generation: failed_generation,
            event: RunEvent::SessionStarted {
                session_id: session_a.id,
                title: session_a.title.clone(),
            },
        })
        .expect("session start before durable user turn");
    controller.drain_runtime_messages();
    assert_eq!(controller.composer_commit_generation, 0);
    assert!(controller.pending_root_submission.is_some());
    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| review.current_draft_text.as_str()),
        Some("edited review")
    );
    controller
        .runtime_tx
        .send(RuntimeMessage::Finished {
            run_generation: failed_generation,
            result: Err("preflight rejected".to_string()),
        })
        .expect("preflight failure");
    controller.drain_runtime_messages();
    assert_eq!(controller.composer_commit_generation, 0);
    assert_eq!(
        controller.state.composer.image_attachment_paths,
        vec![retained_image.clone()]
    );
    assert_eq!(
        desktop_web_state(&controller.state, &DesktopRuntimeProjection::default()).attached_images,
        vec![retained_image.to_string()]
    );
    assert!(
        controller
            .state
            .selected_detail()
            .transcript_rows
            .iter()
            .all(|row| row.row_kind != super::super::models::DesktopTranscriptRowKind::User)
    );
    assert!(controller.pending_root_submission.is_none());
    assert_eq!(
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| review.current_draft_text.as_str()),
        Some("edited review")
    );
    assert_eq!(
        controller.state.view.overlay,
        super::super::state::DesktopOverlay::PromptReview
    );

    let admitted_generation = failed_generation + 1;
    controller
        .run_lifecycle
        .begin(admitted_generation, RunControl::new());
    controller.state.begin_agent_run();
    controller.pending_root_submission = Some(PendingRootSubmission {
        run_generation: admitted_generation,
        owner_workspace_path: root.clone(),
        owner_session_id: Some(session_a.id),
        prompt_dispatch: crate::session::PromptDispatchPart::raw("commit after admission"),
        image_paths: vec![retained_image.clone()],
        prompt_review_to_cancel: Some(review_target),
    });
    let delayed_refresh_target = SessionRefreshRequestTarget {
        workspace_root: root.clone(),
        session_id: session_a.id,
    };
    let delayed_refresh_request = controller
        .session_projection_refresh_requests
        .begin(delayed_refresh_target.clone());
    let delayed_refresh = loaded_session_from_detail(
        load_latest_session_detail(&controller.app, session_a.id)
            .await
            .expect("delayed terminal refresh"),
        None,
    );
    controller
        .runtime_tx
        .send(RuntimeMessage::CurrentSessionRefreshed {
            request_id: delayed_refresh_request,
            target: delayed_refresh_target,
            purpose: CurrentSessionRefreshPurpose::Refresh,
            result: Ok(delayed_refresh),
        })
        .expect("terminal refresh queued before the next session event");
    controller
        .runtime_tx
        .send(RuntimeMessage::RunEvent {
            run_generation: admitted_generation,
            event: RunEvent::SessionStarted {
                session_id: session_a.id,
                title: session_a.title.clone(),
            },
        })
        .expect("admission event");
    controller.drain_runtime_messages();
    assert_eq!(controller.composer_commit_generation, 0);
    assert!(controller.pending_root_submission.is_some());
    assert_eq!(
        controller.state.composer.image_attachment_paths,
        vec![retained_image.clone()]
    );
    assert!(controller.state.app_state.prompt_review.is_some());
    controller
        .runtime_tx
        .send(RuntimeMessage::RunEvent {
            run_generation: admitted_generation,
            event: RunEvent::UserTurnStored {
                session_id: session_a.id,
                turn: Box::new(crate::protocol::UserTurn {
                    turn_id: crate::protocol::TurnId::new(),
                    items: vec![crate::protocol::UserInputItem::Text {
                        text: "durable prompt".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                }),
            },
        })
        .expect("durable user message");
    assert!(controller.cancel_root_run_at_generation(admitted_generation));
    controller.drain_runtime_messages();
    assert_eq!(controller.composer_commit_generation, 1);
    assert!(controller.state.composer.image_attachment_paths.is_empty());
    assert!(controller.pending_root_submission.is_none());
    assert!(controller.state.app_state.prompt_review.is_none());
    assert!(
        desktop_web_state(&controller.state, &DesktopRuntimeProjection::default())
            .review_draft_text
            .is_empty()
    );
    assert_eq!(
        controller.state.view.overlay,
        super::super::state::DesktopOverlay::None
    );
    controller
        .runtime_tx
        .send(RuntimeMessage::Finished {
            run_generation: admitted_generation,
            result: Err("test cleanup".to_string()),
        })
        .expect("cleanup");
    controller.drain_runtime_messages();

    let created_session_id = session_a.id;
    let submitted_image = root.join("submitted-first-run.png");
    let next_image = root.join("next-request.png");
    controller.state.app_state.current_session_id = None;
    controller.state.rebind_composer_owner(None);
    controller.state.composer.image_attachment_paths =
        vec![submitted_image.clone(), next_image.clone()];
    controller.pending_root_submission = Some(PendingRootSubmission {
        run_generation: 902,
        owner_workspace_path: root.clone(),
        owner_session_id: None,
        prompt_dispatch: crate::session::PromptDispatchPart::raw("first run"),
        image_paths: vec![submitted_image],
        prompt_review_to_cancel: None,
    });
    controller.state.app_state.current_session_id = Some(created_session_id);
    assert!(controller.commit_pending_root_submission(902, None));
    assert_eq!(controller.composer_commit_generation, 2);
    assert_eq!(
        controller.state.composer.image_attachment_paths,
        vec![next_image.clone()]
    );
    controller
        .state
        .bind_composer_to_loaded_session(created_session_id);
    assert_eq!(
        controller.state.composer.image_attachment_paths,
        vec![next_image.clone()]
    );
    assert_eq!(
        controller.state.composer.image_attachment_paths,
        vec![next_image]
    );

    let blocker = controller.state.begin_project_delete_mutation();
    assert!(controller.state.finish_project_delete_mutation(blocker));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_access_mode_persistence_keeps_every_runtime_owner_and_permission_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
    std::fs::create_dir_all(&root).expect("workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let paths = crate::storage::StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir,
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
    sqlite.migrate().expect("migrate");
    let store = crate::storage::StoreBundle::new(sqlite);
    let app = build_test_app(&root, store).await;
    let args = DesktopArgs {
        directory: Some(root),
        session_id: None,
        continue_last: false,
        global_config_existed_at_launch: true,
    };
    let mut controller = DesktopController::new_with_preferences_and_persistence(
        app,
        args,
        DesktopPreferences::default(),
        false,
    )
    .await
    .expect("controller");
    assert!(controller.reload_config());
    let initial_access_mode = controller.app.config.permissions.access_mode;
    controller.run_lifecycle.begin(1, RunControl::new());
    let request = test_permission("pending permission");
    let (response, receiver) = mpsc::channel();
    controller.pending_permission = Some(PendingPermission {
        confirmation_id: 42,
        request: request.clone(),
        responder: response,
        run_control: RunControl::new(),
    });

    assert!(!controller.toggle_access_mode_with_persistence(
        |_, _| Err("simulated persistence failure".to_string()),
        |_, _| Ok(()),
    ));

    assert_eq!(
        controller.app.config.permissions.access_mode,
        initial_access_mode
    );
    assert_eq!(
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        initial_access_mode
    );
    assert_eq!(
        controller
            .pending_permission
            .as_ref()
            .map(|pending| pending.confirmation_id),
        Some(42)
    );
    assert_eq!(
        controller
            .pending_permission
            .as_ref()
            .map(|pending| pending.request.summary.as_str()),
        Some(request.summary.as_str())
    );
    let projection = controller.next_web_state().expect("permission projection");
    assert_eq!(projection.confirmation_id.as_deref(), Some("42"));
    assert!(projection.confirmation_visible);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn durable_activity_fallback_is_session_scoped_and_live_rows_win() {
    let root_session_id = SessionId::new();
    let other_session_id = SessionId::new();
    let durable = (
        root_session_id,
        vec![
            agent_record(
                SessionId::new(),
                "/root/research",
                AgentStatus::Completed(Some("research result".to_string())),
                "research result",
            ),
            agent_record(
                SessionId::new(),
                "/root/review",
                AgentStatus::Completed(Some("review result".to_string())),
                "review result",
            ),
            agent_record(
                SessionId::new(),
                "/root/tests",
                AgentStatus::Completed(Some("test result".to_string())),
                "test result",
            ),
        ],
    );

    let restored = activity_records_for_projection(root_session_id, Vec::new(), Some(&durable));
    assert_eq!(restored.len(), 3);
    assert_eq!(
        restored
            .iter()
            .map(|record| record.agent_path.as_str())
            .collect::<Vec<_>>(),
        vec!["/root/research", "/root/review", "/root/tests"]
    );
    assert!(restored.iter().all(|record| {
        matches!(record.status, AgentStatus::Completed(Some(_)))
            && !record.result_preview.is_empty()
    }));
    assert!(
        activity_records_for_projection(other_session_id, Vec::new(), Some(&durable)).is_empty()
    );

    let live = agent_record(SessionId::new(), "/root/live", AgentStatus::Running, "");
    let selected =
        activity_records_for_projection(root_session_id, vec![live.clone()], Some(&durable));
    assert_eq!(selected, vec![live]);
    assert!(agent_activity_projection("C:/workspace", root_session_id, selected).1);
}

#[test]
fn durable_only_running_activity_is_active_and_requests_bounded_refresh() {
    let root_session_id = SessionId::new();
    let running = agent_record(SessionId::new(), "/root/research", AgentStatus::Running, "");
    let durable = (root_session_id, vec![running]);
    let live = Vec::new();
    let projected = activity_records_for_projection(root_session_id, live.clone(), Some(&durable));

    assert!(agent_activity_records_are_active(&projected));
    assert!(durable_agent_activity_refresh_required(
        &live, &projected, false, false,
    ));
    assert!(!durable_agent_activity_refresh_required(
        &live, &projected, true, false,
    ));
    assert!(!durable_agent_activity_refresh_required(
        &live, &projected, false, true,
    ));

    let local_live = vec![agent_record(
        SessionId::new(),
        "/root/live",
        AgentStatus::Running,
        "",
    )];
    assert!(!durable_agent_activity_refresh_required(
        &local_live,
        &local_live,
        false,
        false,
    ));
}

#[test]
fn durable_activity_refresh_completion_is_latest_wins_and_session_scoped() {
    let session_id = SessionId::new();
    let target = SessionRefreshRequestTarget {
        workspace_root: Utf8PathBuf::from("C:/workspace-a"),
        session_id,
    };
    let mut tracker = LatestRequestTracker::default();
    let stale_request = tracker.begin(target.clone());
    let current_request = tracker.begin(target.clone());

    assert!(!finish_durable_agent_activity_refresh_request(
        &mut tracker,
        stale_request,
        &target,
        Utf8Path::new("C:/workspace-a"),
        Some(session_id),
    ));
    assert!(tracker.is_pending());
    assert!(!finish_durable_agent_activity_refresh_request(
        &mut tracker,
        current_request,
        &target,
        Utf8Path::new("C:/workspace-b"),
        Some(session_id),
    ));
    assert!(!tracker.is_pending());

    let wrong_session_request = tracker.begin(target.clone());
    assert!(!finish_durable_agent_activity_refresh_request(
        &mut tracker,
        wrong_session_request,
        &target,
        Utf8Path::new("C:/workspace-a"),
        Some(SessionId::new()),
    ));
    assert!(!tracker.is_pending());

    let accepted_request = tracker.begin(target.clone());
    assert!(finish_durable_agent_activity_refresh_request(
        &mut tracker,
        accepted_request,
        &target,
        Utf8Path::new("C:/workspace-a"),
        Some(session_id),
    ));
    assert!(!finish_durable_agent_activity_refresh_request(
        &mut tracker,
        accepted_request,
        &target,
        Utf8Path::new("C:/workspace-a"),
        Some(session_id),
    ));
}

#[test]
fn cancelled_active_permission_clears_by_id_and_advances_broker() {
    let (control, runtime_rx) = test_desktop_control_plane();
    let broker = SharedConfirmationPrompt::new(DesktopConfirmationPrompt {
        control,
        next_permission_request_id: Arc::new(AtomicU64::new(41)),
    });

    let first_cancel = RunControl::new();
    let (first_done_tx, first_done_rx) = mpsc::sync_channel(1);
    let mut first_prompt = broker.clone();
    let first_wait_cancel = first_cancel.clone();
    std::thread::spawn(move || {
        let result =
            first_prompt.confirm_with_control(&test_permission("first"), &first_wait_cancel);
        let _ = first_done_tx.send(result);
    });

    let (first_id, first_response) = match recv_runtime_message(&runtime_rx) {
        RuntimeMessage::Permission {
            confirmation_id,
            response,
            ..
        } => (confirmation_id, response),
        _ => panic!("expected first desktop permission"),
    };
    first_cancel.interrupt(TurnInterruptionCause::UserStop);
    match recv_runtime_message(&runtime_rx) {
        RuntimeMessage::PermissionCancelled { confirmation_id } => {
            assert_eq!(confirmation_id, first_id)
        }
        _ => panic!("expected desktop permission cancellation"),
    }
    assert!(matches!(
        first_done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("first confirmation result")
            .expect("first confirmation"),
        ConfirmationOutcome::Interrupted
    ));

    let mut pending = Some(PendingPermission {
        confirmation_id: first_id,
        request: test_permission("first"),
        responder: first_response,
        run_control: first_cancel,
    });
    assert!(!clear_cancelled_permission(&mut pending, first_id + 1));
    assert_eq!(
        pending.as_ref().map(|pending| pending.confirmation_id),
        Some(first_id)
    );
    assert!(clear_cancelled_permission(&mut pending, first_id));
    assert!(pending.is_none());

    let (second_done_tx, second_done_rx) = mpsc::sync_channel(1);
    let mut second_prompt = broker;
    std::thread::spawn(move || {
        let result = second_prompt.confirm(&test_permission("second"));
        let _ = second_done_tx.send(result);
    });
    let (second_id, second_response) = match recv_runtime_message(&runtime_rx) {
        RuntimeMessage::Permission {
            confirmation_id,
            response,
            ..
        } => (confirmation_id, response),
        _ => panic!("expected second desktop permission"),
    };
    assert!(second_id > first_id);
    second_response
        .send(ReviewDecision::Approved)
        .expect("answer second permission");
    assert_eq!(
        second_done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("second confirmation result")
            .expect("second confirmation"),
        ReviewDecision::Approved
    );
}

#[test]
fn disconnected_control_plane_rejects_bootstrap_and_permission() {
    let (runtime_tx, _runtime_rx) = mpsc::sync_channel(1);
    let (control_tx, control_rx) = mpsc::channel();
    drop(control_rx);
    let disconnected = DesktopControlPlaneSender { tx: control_tx };
    let mut renderer = DesktopRenderer {
        runtime_tx,
        bootstrap_control: disconnected.clone(),
        run_generation: 42,
        notification_title: "test".to_string(),
        notified_terminal: false,
        control_result: None,
    };
    let bootstrap_error = renderer
        .render(&RunEvent::SessionStarted {
            session_id: SessionId::new(),
            title: "unavailable".to_string(),
        })
        .expect_err("disconnected bootstrap must fail");
    assert!(
        bootstrap_error
            .to_string()
            .contains("control mailbox is unavailable")
    );

    let mut prompt = DesktopConfirmationPrompt {
        control: disconnected,
        next_permission_request_id: Arc::new(AtomicU64::new(1)),
    };
    let permission_error = prompt
        .confirm(&test_permission("disconnected"))
        .expect_err("disconnected permission must fail");
    assert!(
        permission_error
            .to_string()
            .contains("control mailbox is unavailable")
    );
}

#[test]
fn desktop_permission_abort_is_ticket_local_and_loses_to_existing_cause() {
    let (control, runtime_rx) = test_desktop_control_plane();
    let mut prompt = DesktopConfirmationPrompt {
        control,
        next_permission_request_id: Arc::new(AtomicU64::new(61)),
    };
    let control = RunControl::new();
    let observer = control.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = prompt.confirm_with_control(&test_permission("abort"), &control);
        let _ = done_tx.send(result);
    });
    let response = match recv_runtime_message(&runtime_rx) {
        RuntimeMessage::Permission { response, .. } => response,
        _ => panic!("expected Desktop permission"),
    };
    response
        .send(ReviewDecision::Abort)
        .expect("send ticket-local abort");
    assert_eq!(
        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("abort result")
            .expect("abort outcome"),
        ConfirmationOutcome::AbortRequested
    );
    assert_eq!(observer.cause(), None);

    let (control, runtime_rx) = test_desktop_control_plane();
    let mut prompt = DesktopConfirmationPrompt {
        control,
        next_permission_request_id: Arc::new(AtomicU64::new(71)),
    };
    let control = RunControl::new();
    let observer = control.clone();
    let worker_control = control.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result =
            prompt.confirm_with_control(&test_permission("competing abort"), &worker_control);
        let _ = done_tx.send(result);
    });
    let response = match recv_runtime_message(&runtime_rx) {
        RuntimeMessage::Permission { response, .. } => response,
        _ => panic!("expected competing Desktop permission"),
    };
    assert!(control.fail("provider failed first"));
    response
        .send(ReviewDecision::Abort)
        .expect("send losing abort");
    assert_eq!(
        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("competing result")
            .expect("competing outcome"),
        ConfirmationOutcome::Interrupted
    );
    assert_eq!(
        observer.cause(),
        Some(RunCancellationCause::Failure(
            "provider failed first".to_string()
        ))
    );
}

#[test]
fn projection_revision_is_monotonic_at_the_command_owner() {
    let mut revision = 0;
    assert_eq!(advance_projection_revision(&mut revision), Ok(1));
    assert_eq!(advance_projection_revision(&mut revision), Ok(2));

    revision = (1_u64 << 53) - 1;
    assert_eq!(advance_projection_revision(&mut revision), Ok(1_u64 << 53));
    assert_eq!(
        projection_revision_text((1_u64 << 53) - 1),
        "9007199254740991"
    );
    assert_eq!(projection_revision_text(1_u64 << 53), "9007199254740992");
    assert_eq!(projection_revision_text(u64::MAX), "18446744073709551615");

    revision = u64::MAX;
    assert!(advance_projection_revision(&mut revision).is_err());
    assert_eq!(revision, u64::MAX);
}

#[test]
fn attachment_authorization_diff_revokes_only_paths_no_longer_projected() {
    let first = Utf8PathBuf::from("C:/outside/first.png");
    let retained = Utf8PathBuf::from("C:/outside/retained.png");
    let mut authorized = BTreeSet::from([first.clone(), retained.clone()]);
    let desired = BTreeSet::from([retained]);

    assert_eq!(
        attachment_authorizations_to_revoke(&authorized, &desired),
        vec![first.clone()]
    );
    authorized.remove(&first);
    assert_eq!(
        attachment_authorizations_to_revoke(&authorized, &desired),
        Vec::<Utf8PathBuf>::new(),
        "a successful revoke must not be issued again"
    );

    let workspace_replacement = attachment_authorizations_to_revoke(&authorized, &BTreeSet::new());
    assert_eq!(
        workspace_replacement,
        authorized.into_iter().collect::<Vec<_>>()
    );
}

#[test]
fn session_delete_completion_is_bound_to_request_and_workspace_identity() {
    let project_id = ProjectId::new();
    let mut state = DesktopState::new(
        super::super::models::DesktopSnapshot {
            workspace_path: "C:/workspace-a".to_string(),
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
    let target = SessionDeleteRequestTarget {
        workspace_root: Utf8PathBuf::from("C:/workspace-a"),
        project_id,
        session_id: SessionId::new(),
        operation_id: state.begin_session_delete_mutation(),
    };

    assert!(session_delete_target_matches(
        &target,
        Utf8Path::new("C:/workspace-a"),
        project_id,
    ));
    assert!(!session_delete_target_matches(
        &target,
        Utf8Path::new("C:/workspace-b"),
        project_id,
    ));
    assert!(!session_delete_target_matches(
        &target,
        Utf8Path::new("C:/workspace-a"),
        ProjectId::new(),
    ));
    assert!(!finish_session_delete_request(
        &mut state,
        &target,
        Utf8Path::new("C:/workspace-b"),
        project_id,
    ));
    assert!(state.background_mutation_pending());
    assert!(finish_session_delete_request(
        &mut state,
        &target,
        Utf8Path::new("C:/workspace-a"),
        project_id,
    ));
    assert!(!state.background_mutation_pending());
    assert!(!finish_session_delete_request(
        &mut state,
        &target,
        Utf8Path::new("C:/workspace-a"),
        project_id,
    ));
}

#[test]
fn history_export_completion_rejects_stale_request_workspace_and_repeat() {
    let session_id = SessionId::new();
    let target = HistoryExportRequestTarget {
        workspace_authority_root: Utf8PathBuf::from("C:/repo/bbb"),
        session_id,
    };
    let mut tracker = LatestRequestTracker::default();
    let stale_request = tracker.begin(target.clone());
    let current_request = tracker.begin(target.clone());

    assert_eq!(
        finish_history_export_request(
            &mut tracker,
            stale_request,
            &target,
            Utf8Path::new("C:/repo/bbb"),
        ),
        None,
        "an older completion cannot settle the latest export owner"
    );
    assert_eq!(
        finish_history_export_request(
            &mut tracker,
            current_request,
            &target,
            Utf8Path::new("C:/repo/ccc"),
        ),
        Some(false),
        "a current request from a replaced sibling authority cannot update status"
    );
    assert_eq!(
        finish_history_export_request(
            &mut tracker,
            current_request,
            &target,
            Utf8Path::new("C:/repo/bbb"),
        ),
        None,
        "the same completion is consumed at most once"
    );
}

#[test]
fn renderer_leaves_an_ordinary_committed_assistant_to_the_canonical_cursor() {
    let (runtime_tx, runtime_rx) = mpsc::sync_channel(1);
    let (control, control_rx) = test_desktop_control_plane();
    let mut renderer = DesktopRenderer {
        runtime_tx,
        bootstrap_control: control,
        run_generation: 77,
        notification_title: "test".to_string(),
        notified_terminal: false,
        control_result: None,
    };

    renderer
        .render(&RunEvent::AssistantMessageCommitted {
            response_id: crate::protocol::ModelResponseId::new(),
            text: "canonical only".to_string(),
        })
        .expect("committed assistant renderer observation");

    assert!(matches!(
        runtime_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(matches!(
        control_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn renderer_allows_runtime_only_delta_loss_when_the_mailbox_is_full() {
    let run_generation = 78;
    let response_id = crate::protocol::ModelResponseId::new();
    let (runtime_tx, runtime_rx) = mpsc::sync_channel(1);
    runtime_tx
        .try_send(RuntimeMessage::RunEvent {
            run_generation,
            event: RunEvent::TextDelta {
                response_id,
                delta: "retained".to_string(),
            },
        })
        .expect("fill runtime mailbox");
    let (control, control_rx) = test_desktop_control_plane();
    let mut renderer = DesktopRenderer {
        runtime_tx,
        bootstrap_control: control,
        run_generation,
        notification_title: "test".to_string(),
        notified_terminal: false,
        control_result: None,
    };

    renderer
        .render(&RunEvent::TextDelta {
            response_id,
            delta: "dropped".to_string(),
        })
        .expect("runtime-only saturation is lossy by contract");

    assert!(matches!(
        control_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(matches!(
        runtime_rx.recv().expect("retained delta"),
        RuntimeMessage::RunEvent {
            event: RunEvent::TextDelta { delta, .. },
            ..
        } if delta == "retained"
    ));
    assert!(matches!(
        runtime_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn bootstrap_and_worker_settlement_bypass_a_full_runtime_mailbox_in_fifo_order() {
    let run_generation = 79;
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let response_id = crate::protocol::ModelResponseId::new();
    let summary = RunSummary::from_terminal(
        session_id,
        turn_id,
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: Some(response_id),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
    );
    let (runtime_tx, runtime_rx) = mpsc::sync_channel(1);
    runtime_tx
        .try_send(RuntimeMessage::RunEvent {
            run_generation,
            event: RunEvent::TextDelta {
                response_id,
                delta: "mailbox backlog".to_string(),
            },
        })
        .expect("fill runtime mailbox");
    let (control, control_rx) = test_desktop_control_plane();
    let mut renderer = DesktopRenderer {
        runtime_tx,
        bootstrap_control: control.clone(),
        run_generation,
        notification_title: "test".to_string(),
        notified_terminal: false,
        control_result: None,
    };

    renderer
        .render(&RunEvent::SessionStarted {
            session_id,
            title: "new session".to_string(),
        })
        .expect("lossless session bootstrap");
    renderer
        .render(&RunEvent::UserTurnStored {
            session_id,
            turn: Box::new(UserTurn {
                turn_id,
                items: vec![UserInputItem::Text {
                    text: "durable prompt".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            }),
        })
        .expect("lossless user-turn bootstrap");
    publish_desktop_run_finished(&control, run_generation, Ok(summary));

    assert!(matches!(
        control_rx.recv().expect("session bootstrap"),
        RuntimeMessage::RunEvent {
            run_generation: 79,
            event: RunEvent::SessionStarted { session_id: received, .. },
        } if received == session_id
    ));
    assert!(matches!(
        control_rx.recv().expect("user-turn bootstrap"),
        RuntimeMessage::RunEvent {
            run_generation: 79,
            event: RunEvent::UserTurnStored { session_id: received, .. },
        } if received == session_id
    ));
    assert!(matches!(
        control_rx.recv().expect("worker settlement"),
        RuntimeMessage::Finished {
            run_generation: 79,
            result: Ok(received),
        } if received.session_id() == session_id
    ));
    assert!(matches!(
        control_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(matches!(
        runtime_rx.recv().expect("unchanged runtime backlog"),
        RuntimeMessage::RunEvent {
            event: RunEvent::TextDelta { delta, .. },
            ..
        } if delta == "mailbox backlog"
    ));
}

#[tokio::test]
async fn controller_drains_lossless_control_before_a_runtime_backlog() {
    let (_temp, _root, mut controller) = empty_access_test_controller().await;
    let run_generation = 80;
    let response_id = crate::protocol::ModelResponseId::new();
    let summary = RunSummary::from_terminal(
        SessionId::new(),
        TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: Some(response_id),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
    );
    controller
        .run_lifecycle
        .begin(run_generation, RunControl::new());
    controller.state.begin_agent_run();
    for _ in 0..DESKTOP_RUNTIME_MAILBOX_CAPACITY {
        controller
            .runtime_tx
            .try_send(RuntimeMessage::RunEvent {
                run_generation,
                event: RunEvent::TextDelta {
                    response_id,
                    delta: String::new(),
                },
            })
            .expect("fill runtime mailbox exactly");
    }
    publish_desktop_run_finished(&controller.control_tx, run_generation, Ok(summary));

    controller.drain_runtime_messages();

    assert!(
        !controller.run_lifecycle.root_is_active(),
        "control settlement must be admitted before the first bounded runtime drain"
    );
}

#[tokio::test]
async fn steer_settlement_bypasses_a_full_runtime_mailbox() {
    let (_temp, root, mut controller) = empty_access_test_controller().await;
    let session_id = SessionId::new();
    controller.state.app_state.current_session_id = Some(session_id);
    let attachment = root.join("steer.png");
    controller
        .state
        .composer
        .image_attachment_paths
        .push(attachment.clone());
    let target = SteerSubmissionTarget {
        operation_id: controller.state.begin_steer_submission(),
        workspace_root: root,
        session_id,
        expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
    };
    let response_id = crate::protocol::ModelResponseId::new();
    for _ in 0..DESKTOP_RUNTIME_MAILBOX_CAPACITY {
        controller
            .runtime_tx
            .try_send(RuntimeMessage::RunEvent {
                run_generation: 999,
                event: RunEvent::TextDelta {
                    response_id,
                    delta: String::new(),
                },
            })
            .expect("fill runtime mailbox exactly");
    }
    controller
        .control_tx
        .send(RuntimeMessage::SteerFinished {
            target,
            image_paths: vec![attachment],
            result: Ok(()),
        })
        .expect("publish steer settlement");

    controller.drain_runtime_messages();

    assert!(!controller.state.steer_submission_pending());
    assert!(controller.state.composer.image_attachment_paths.is_empty());
    assert_eq!(controller.composer_commit_generation, 1);
}

#[test]
fn committed_assistant_requires_a_canonical_projection_refresh() {
    assert!(live_event_requires_canonical_refresh(
        &RunEvent::AssistantMessageCommitted {
            response_id: crate::protocol::ModelResponseId::new(),
            text: "canonical response".to_string(),
        }
    ));
}
