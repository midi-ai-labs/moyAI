use camino::Utf8PathBuf;

use super::*;
use crate::config::{AccessMode, ProviderProfile};
use crate::harness::{HarnessRunId, HarnessRunRecord, HarnessRunStatus, HarnessRunStore};
use crate::protocol::{
    ContentPart, InterAgentCommunication, ModeKind, ProtocolEventStore, ToolLifecycleStatus,
    UserInputItem,
};
use crate::session::{
    ChangeId, ChangeKind, ChangeRepository, NewSession, ProjectRepository,
    SessionProviderConnection,
};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

async fn test_repo() -> (StoreBundle, SessionId) {
    let temp = tempfile::tempdir().expect("tempdir");
    let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 path");
    let paths = StoragePaths {
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
        data_dir: data_dir.clone(),
    };
    let sqlite = SqliteStore::open(&paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let project_id = ProjectId::new();
    store
        .project_repo()
        .upsert_project(project_id, &data_dir, "test", "none")
        .await
        .expect("project");
    let session = store
        .session_repo()
        .create_session(NewSession {
            project_id,
            title: "test".to_string(),
            cwd: data_dir,
            model: "model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            access_mode: AccessMode::Default,
            provider_connection: None,
        })
        .await
        .expect("session");
    (store, session.id)
}

#[tokio::test]
async fn clm_checkpoint_commit_reopens_and_stale_admission_cannot_replace_it() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let original = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .unwrap();
    let checkpoint = crate::protocol::ClmCheckpoint::from_messages(
        &[
            crate::llm::ModelMessage::User {
                content: "canonical request".into(),
            },
            crate::llm::ModelMessage::Assistant {
                content: "retained CLM note".into(),
            },
        ],
        1,
    )
    .unwrap();
    let event = RunEvent::CompactionCompleted {
        summarized_messages: 1,
        layout: crate::protocol::CompactionLayout::ClmCheckpoint,
        clm_checkpoint: Some(checkpoint.clone()),
        preserved_user_messages: Vec::new(),
        summary: "Context updated".into(),
        replacement_item_ids: vec![original[0].id],
    };
    let repository = store.session_repo();
    for (layout, payload, anchors) in [
        (
            crate::protocol::CompactionLayout::ClmCheckpoint,
            None,
            Vec::new(),
        ),
        (
            crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
            Some(checkpoint.clone()),
            Vec::new(),
        ),
        (
            crate::protocol::CompactionLayout::ClmCheckpoint,
            Some(checkpoint.clone()),
            vec!["duplicate anchor".into()],
        ),
    ] {
        let invalid = RunEvent::CompactionCompleted {
            summarized_messages: 1,
            layout,
            clm_checkpoint: payload,
            preserved_user_messages: anchors,
            summary: "Context updated".into(),
            replacement_item_ids: vec![original[0].id],
        };
        repository
            .commit_admitted_compaction_with_protocol_bundle(
                session_id,
                admission_id,
                &invalid,
                turn_id,
                None,
            )
            .await
            .expect_err("invalid snapshot/layout must not commit");
    }
    assert_eq!(
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .unwrap()
            .len(),
        original.len()
    );
    repository
        .commit_admitted_compaction_with_protocol_bundle(
            session_id,
            admission_id,
            &event,
            turn_id,
            None,
        )
        .await
        .unwrap();
    let before = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .unwrap();
    let reopened_sqlite = SqliteStore::open(store.paths()).unwrap();
    reopened_sqlite
        .migrate()
        .expect("typed snapshot is accepted on reopen");
    let reopened = StoreBundle::new(reopened_sqlite);
    let after = reopened
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&after[0]).unwrap(),
        serde_json::to_value(&original[0]).unwrap()
    );
    let HistoryItemPayload::Compaction {
        clm_checkpoint: Some(restored),
        summary,
        ..
    } = &after[1].payload
    else {
        panic!("CLM checkpoint")
    };
    assert_eq!(summary, "Context updated");
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(checkpoint).unwrap()
    );
    repository
        .record_agent_tree_stop_fence(session_id, crate::protocol::TurnInterruptionCause::UserStop)
        .await
        .unwrap();
    repository
        .commit_admitted_compaction_with_protocol_bundle(
            session_id,
            admission_id,
            &event,
            turn_id,
            None,
        )
        .await
        .expect_err("stopped admission cannot append a snapshot");
    assert_eq!(
        serde_json::to_value(
            store
                .protocol_event_store()
                .list_history_items(session_id, turn_id)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(before).unwrap()
    );
}

#[tokio::test]
async fn origin_deletion_scan_keeps_accepted_job_ids_and_rejects_incomplete_history() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let project_id = repository
        .get_session(session_id)
        .await
        .expect("session")
        .project_id;
    assert_eq!(
        repository
            .project_session_ids_for_origin_guard(project_id, 1)
            .expect("project session IDs"),
        vec![session_id]
    );
    assert!(
        repository
            .project_session_ids_for_origin_guard(project_id, 0)
            .is_err()
    );
    assert!(
        !repository
            .session_attempted_team_delegation(session_id)
            .expect("no delegation")
    );
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();
    let call = HistoryItemPayload::ToolCall {
        call_id,
        response_id: ModelResponseId::new(),
        model_call_id: "call-provider".into(),
        tool_name: "team_delegate".into(),
        arguments_json: "{}".into(),
    };
    let output = HistoryItemPayload::ToolOutput {
        call_id,
        status: ToolLifecycleStatus::Completed,
        title: "PCへ仕事を依頼".into(),
        output_text: "accepted".into(),
        metadata: serde_json::json!({"tool_metadata":{"job_id":"job-a","project_id":"project-a"},"success":true}),
        success: Some(true),
    };
    {
        let connection = repository.connection.lock().expect("connection");
        for (index, payload) in [call, output].into_iter().enumerate() {
            let json = serde_json::to_string(&payload).expect("payload");
            let sha = format!("{:x}", Sha256::digest(json.as_bytes()));
            connection
                    .execute(
                        "INSERT INTO protocol_history_items
                         (id,session_id,scope_kind,turn_id,sequence_no,payload_json,payload_sha256,created_at_ms)
                         VALUES (?1,?2,'turn',?3,?4,?5,?6,1)",
                        params![
                            ulid::Ulid::new().to_string(),
                            session_id.to_string(),
                            turn_id.to_string(),
                            index as i64,
                            json,
                            sha
                        ],
                    )
                    .expect("canonical row");
        }
    }
    assert!(
        repository
            .session_attempted_team_delegation(session_id)
            .expect("delegation evidence")
    );
    assert_eq!(
        repository
            .origin_delegated_job_ids(session_id)
            .expect("accepted job IDs"),
        vec!["job-a".to_string()]
    );
    let missing_id = HistoryItemPayload::ToolOutput {
        call_id,
        status: ToolLifecycleStatus::Completed,
        title: "bad result".into(),
        output_text: "accepted".into(),
        metadata: serde_json::json!({"tool_metadata":{},"success":true}),
        success: Some(true),
    };
    let replacement = serde_json::to_string(&missing_id).expect("replacement");
    let replacement_sha = format!("{:x}", Sha256::digest(replacement.as_bytes()));
    repository
        .connection
        .lock()
        .expect("connection")
        .execute(
            "UPDATE protocol_history_items SET payload_json = ?1, payload_sha256 = ?2
                 WHERE session_id = ?3 AND sequence_no = 1",
            params![replacement, replacement_sha, session_id.to_string()],
        )
        .expect("replace result");
    assert!(repository.origin_delegated_job_ids(session_id).is_err());
}

async fn create_sibling_session(
    store: &StoreBundle,
    root_session_id: SessionId,
    title: &str,
) -> SessionRecord {
    let draft = sibling_session_draft(store, root_session_id, title).await;
    store
        .session_repo()
        .create_session(draft)
        .await
        .expect("sibling session")
}

async fn sibling_session_draft(
    store: &StoreBundle,
    root_session_id: SessionId,
    title: &str,
) -> NewSession {
    let root = store
        .session_repo()
        .get_session(root_session_id)
        .await
        .expect("root session");
    NewSession {
        project_id: root.project_id,
        title: title.to_string(),
        cwd: root.cwd,
        model: root.model,
        base_url: root.base_url,
        access_mode: root.access_mode,
        provider_connection: root.provider_connection,
    }
}

async fn spawn_pending_child(
    store: &StoreBundle,
    root_session_id: SessionId,
    task_name: &str,
) -> (SessionRecord, HistoryItemId, TurnId) {
    let root_turn_id = TurnId::new();
    let root_admission = store
        .session_repo()
        .admit_session_turn(root_session_id, root_turn_id)
        .await
        .expect("root admission")
        .expect("root admitted");
    let child_session_id = SessionId::new();
    let child_draft = sibling_session_draft(store, root_session_id, task_name).await;
    let child_path = format!("/root/{task_name}");
    let initial_task = InterAgentCommunication {
        author: "/root".to_string(),
        recipient: child_path.clone(),
        content: render_inter_agent_message(
            InterAgentMessageType::NewTask,
            &child_path,
            "/root",
            "run the bounded task",
        ),
        trigger_turn: true,
    };
    let stored = store
        .session_repo()
        .create_agent_spawn_with_initial_task_for_caller_turn(
            root_session_id,
            root_session_id,
            child_session_id,
            child_draft,
            &child_path,
            task_name,
            root_admission.admission_id,
            root_turn_id,
            SpawnContextFork::None,
            initial_task,
        )
        .expect("atomic pending child spawn");
    (
        stored.child_session,
        stored.initial_task_history_item_id,
        root_turn_id,
    )
}

fn pre_admission_failed_terminal(error: &str) -> DurableTurnTerminal {
    DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Failed {
            error: error.to_string(),
        },
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    }
}

fn pre_admission_interrupted_terminal() -> DurableTurnTerminal {
    DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::TreeStopped,
        },
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    }
}

fn pre_admission_user_stopped_terminal() -> DurableTurnTerminal {
    DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::UserStop,
        },
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    }
}

fn pre_admission_agent_interrupted_terminal() -> DurableTurnTerminal {
    DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::AgentInterrupted,
        },
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    }
}

async fn nested_owner_resume_fixture(
    store: &StoreBundle,
    root_session_id: SessionId,
    child_count: usize,
) -> (SessionRecord, Vec<OwnerResumeRequest>) {
    let repository = store.session_repo();
    let owner = create_sibling_session(store, root_session_id, "owner").await;
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            owner.id,
            "/root/owner",
            "owner",
        )
        .await
        .expect("owner edge");
    for index in 0..child_count {
        let task_name = format!("child_{index}");
        let agent_path = format!("/root/owner/{task_name}");
        let child = create_sibling_session(store, root_session_id, &task_name).await;
        repository
            .insert_session_spawn_edge(root_session_id, owner.id, child.id, &agent_path, &task_name)
            .await
            .expect("nested child edge");
        let (admission_id, turn_id) = active_turn(store, child.id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    child.id,
                    admission_id,
                    &failed_terminal(child.id, "child failed"),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("child terminal"),
            AdmittedTerminalCommit::Applied
        );
        // This fixture exercises the OwnerResume state machine itself. Current Codex-aligned
        // completion delivery leaves an idle owner dormant unless a previously deferred
        // owner generation was released, so seed the otherwise-valid durable wake explicitly
        // from the direct-child FINAL handoff.
        let handoff = repository
            .agent_completion_handoff(child.id, turn_id)
            .expect("child completion handoff")
            .expect("stored child completion handoff");
        let now = normalize_run_lease_now_ms(SystemClock::now_ms());
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute(
                "INSERT OR IGNORE INTO agent_owner_resume_requests (
                         owner_session_id,
                         source_session_id,
                         source_history_item_id,
                         state,
                         claimed_turn_id,
                         created_at_ms,
                         updated_at_ms,
                         claimed_at_ms,
                         resolved_at_ms
                     )
                     VALUES (?1, ?1, ?2, 'pending', NULL, ?3, ?3, NULL, NULL)",
                params![
                    owner.id.to_string(),
                    handoff.history_item_id.to_string(),
                    now
                ],
            )
            .expect("seed valid OwnerResume wake");
    }
    let requests = repository
        .list_pending_owner_resume_requests(owner.id)
        .expect("owner resume requests");
    assert_eq!(requests.len(), child_count);
    (owner, requests)
}

async fn retained_test_agent(
    store: &StoreBundle,
    root_session_id: SessionId,
    parent_session_id: SessionId,
    parent_path: &str,
    task_name: &str,
) -> SessionRecord {
    let session = create_sibling_session(store, root_session_id, task_name).await;
    let path = format!("{parent_path}/{task_name}");
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            parent_session_id,
            session.id,
            &path,
            task_name,
        )
        .await
        .expect("retained test-agent edge");
    session
}

fn agent_interrupted_terminal(session_id: SessionId) -> RunEvent {
    RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::AgentInterrupted,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    }
}

fn tree_stopped_terminal(session_id: SessionId) -> RunEvent {
    RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::TreeStopped,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    }
}

async fn orphan_crash_explicit_fixture(
    task_name: &str,
) -> (StoreBundle, SessionRecord, TurnId, StoredAgentCommunication) {
    let (store, root_session_id) = test_repo().await;
    let owner =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", task_name).await;
    let owner_path = format!("/root/{task_name}");
    let child = retained_test_agent(&store, root_session_id, owner.id, &owner_path, "child").await;
    let (_owner_admission, crashed_turn) = active_turn(&store, owner.id).await;
    let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
    let repository = store.session_repo();
    let target = repository
        .captured_running_terminal_target(owner.id)
        .await
        .expect("capture orphan crash owner")
        .expect("running orphan crash owner");
    assert!(
        repository
            .recover_captured_running_session_with_protocol_event(
                owner.id,
                &failed_terminal(owner.id, "owner process crashed"),
                target,
            )
            .await
            .expect("recover orphan crash owner")
    );
    let explicit = repository
        .append_inter_agent_communication_with_protocol_bundle(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: owner_path.clone(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    &format!("/root/{task_name}"),
                    "/root",
                    "recover orphan crash",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("orphan crash explicit trigger");
    assert!(explicit.schedule_turn);
    (store, owner, crashed_turn, explicit)
}

#[tokio::test]
async fn root_completed_terminal_is_independent_of_live_descendants() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (child_admission, child_turn) = active_turn(&store, child.id).await;
    let (root_admission, root_turn) = active_turn(&store, root_session_id).await;
    let repository = store.session_repo();

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                root_admission,
                &completed_terminal_for_response(root_session_id, None),
                root_turn,
                None,
                None,
            )
            .await
            .expect("root terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(matches!(
        repository
            .durable_terminal_for_turn(root_session_id, root_turn)
            .await
            .expect("root durable terminal")
            .expect("root terminal evidence")
            .outcome,
        TurnTerminalOutcome::Completed
    ));
    assert_eq!(
        repository
            .get_session(root_session_id)
            .await
            .expect("root session")
            .status,
        SessionStatus::Completed
    );
    assert_eq!(
        repository
            .get_session(child.id)
            .await
            .expect("live child")
            .status,
        SessionStatus::Running,
        "root success must not cancel or terminalize an active child"
    );

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission,
                &completed_terminal_for_response(child.id, None),
                child_turn,
                None,
                None,
            )
            .await
            .expect("late child terminal"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = repository
        .agent_completion_handoff(child.id, child_turn)
        .expect("late child handoff")
        .expect("late child result remains attached to its exact direct parent");
    assert_eq!(handoff.parent_session_id, root_session_id);
    assert!(matches!(
        repository
            .durable_terminal_for_turn(root_session_id, root_turn)
            .await
            .expect("root terminal reread")
            .expect("root terminal remains")
            .outcome,
        TurnTerminalOutcome::Completed
    ));
    assert!(
        repository
            .schedulable_owner_resume_request_id(root_session_id)
            .expect("root owner-resume lookup")
            .is_none(),
        "a late child result must not rewrite or reopen the terminal root"
    );
}

#[tokio::test]
async fn nonroot_completed_terminal_handoffs_immediately_and_keeps_grandchild_live() {
    let (store, root_session_id) = test_repo().await;
    let owner =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
    let grandchild =
        retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "slow").await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (grandchild_admission, grandchild_turn) = active_turn(&store, grandchild.id).await;
    let repository = store.session_repo();

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, None),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("owner terminal"),
        AdmittedTerminalCommit::Applied
    );
    let owner_effects = repository
        .agent_terminal_effects(owner.id, owner_turn)
        .expect("owner terminal effects");
    assert!(
        owner_effects.deferred.is_none(),
        "normal completion no longer creates a descendant-liveness receipt"
    );
    let owner_handoff = owner_effects
        .completion_handoff
        .expect("owner result must reach its exact direct parent immediately");
    assert_eq!(owner_handoff.parent_session_id, root_session_id);
    assert_eq!(
        repository
            .get_session(grandchild.id)
            .await
            .expect("slow grandchild")
            .status,
        SessionStatus::Running
    );

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                grandchild.id,
                grandchild_admission,
                &completed_terminal_for_response(grandchild.id, None),
                grandchild_turn,
                None,
                None,
            )
            .await
            .expect("late grandchild terminal"),
        AdmittedTerminalCommit::Applied
    );
    let late_handoff = repository
        .agent_completion_handoff(grandchild.id, grandchild_turn)
        .expect("late grandchild handoff")
        .expect("late result remains durably attached to the completed direct parent");
    assert_eq!(late_handoff.parent_session_id, owner.id);
    assert!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("completed owner resume lookup")
            .is_none(),
        "queue-only FINAL must not reopen a completed parent"
    );
    assert!(matches!(
        repository
            .durable_terminal_for_turn(owner.id, owner_turn)
            .await
            .expect("owner terminal reread")
            .expect("owner terminal remains")
            .outcome,
        TurnTerminalOutcome::Completed
    ));
}

#[tokio::test]
async fn agent_interrupted_parent_retains_late_child_final_without_owner_resume() {
    let (store, root_session_id) = test_repo().await;
    let owner =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
    let child =
        retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (child_admission, child_turn) = active_turn(&store, child.id).await;
    let repository = store.session_repo();

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &agent_interrupted_terminal(owner.id),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("interrupt reusable owner"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission,
                &completed_terminal_for_response(child.id, None),
                child_turn,
                None,
                None,
            )
            .await
            .expect("late child result"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = repository
        .agent_completion_handoff(child.id, child_turn)
        .expect("late child handoff")
        .expect("AgentInterrupted parent retains child FINAL");
    assert_eq!(handoff.parent_session_id, owner.id);
    assert!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("owner resume lookup")
            .is_none(),
        "AgentInterrupted requires an explicit follow-up, not an automatic resume"
    );

    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen interrupted owner");
    reopened_sqlite.migrate().expect("migrate reopened store");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert!(
        reopened
            .session_repo()
            .agent_completion_handoff(child.id, child_turn)
            .expect("reopened late child handoff")
            .is_some()
    );
    assert!(
        reopened
            .session_repo()
            .schedulable_owner_resume_request_id(owner.id)
            .expect("reopened owner resume lookup")
            .is_none()
    );
}

#[tokio::test]
async fn agent_interruption_releases_deferred_final_to_cancelled_parent_without_rollback() {
    let (store, root_session_id) = test_repo().await;
    let parent =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "parent").await;
    let owner =
        retained_test_agent(&store, root_session_id, parent.id, "/root/parent", "owner").await;
    let leaf = retained_test_agent(
        &store,
        root_session_id,
        owner.id,
        "/root/parent/owner",
        "leaf",
    )
    .await;
    let (parent_admission, parent_turn) = active_turn(&store, parent.id).await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
    let repository = store.session_repo();

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                parent.id,
                parent_admission,
                &agent_interrupted_terminal(parent.id),
                parent_turn,
                None,
                None,
            )
            .await
            .expect("interrupt reusable parent"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, None),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("defer early owner completion"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(&store, owner.id, owner_turn, parent.id);
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                leaf.id,
                leaf_admission,
                &agent_interrupted_terminal(leaf.id),
                leaf_turn,
                None,
                None,
            )
            .await
            .expect("leaf AgentInterrupted must commit"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, owner_turn)
            .expect("released owner effects")
            .deferred
            .expect("owner deferred receipt")
            .state,
        DeferredAgentCompletionState::Released
    );
    let _handoff = repository
        .agent_completion_handoff(owner.id, owner_turn)
        .expect("released owner handoff")
        .expect("forensic FINAL to cancelled parent");
    assert!(
        repository
            .schedulable_owner_resume_request_id(parent.id)
            .expect("cancelled parent owner resume")
            .is_none()
    );
    assert!(
        repository
            .durable_terminal_for_turn(leaf.id, leaf_turn)
            .await
            .expect("leaf durable terminal")
            .is_some(),
        "deferred release must not roll back the resolver terminal"
    );
}

#[tokio::test]
async fn tree_stop_before_late_nested_result_discards_deferred_and_survives_restart() {
    let (store, root_session_id) = test_repo().await;
    let middle =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "middle").await;
    let leaf =
        retained_test_agent(&store, root_session_id, middle.id, "/root/middle", "leaf").await;
    let (root_admission, root_turn) = active_turn(&store, root_session_id).await;
    let (middle_admission, middle_turn) = active_turn(&store, middle.id).await;
    let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
    let repository = store.session_repo();

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                middle.id,
                middle_admission,
                &completed_terminal_for_response(middle.id, None),
                middle_turn,
                None,
                None,
            )
            .await
            .expect("middle early completion"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(
        &store,
        middle.id,
        middle_turn,
        root_session_id,
    );
    let root_stop = RunEvent::TurnTerminal {
        session_id: root_session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert!(
        repository
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("record explicit root tree-Stop boundary")
            .is_some()
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                root_admission,
                &root_stop,
                root_turn,
                None,
                None,
            )
            .await
            .expect("durable root UserStop"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                leaf.id,
                leaf_admission,
                &completed_terminal_for_response(leaf.id, None),
                leaf_turn,
                None,
                None,
            )
            .await
            .expect("late leaf completion"),
        AdmittedTerminalCommit::NotOwned
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                leaf.id,
                leaf_admission,
                &tree_stopped_terminal(leaf.id),
                leaf_turn,
                None,
                None,
            )
            .await
            .expect("compatible stopped leaf terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        repository
            .agent_completion_handoff(leaf.id, leaf_turn)
            .expect("late leaf handoff lookup")
            .is_none()
    );
    assert_eq!(
        repository
            .agent_terminal_effects(middle.id, middle_turn)
            .expect("middle stopped effects")
            .deferred
            .expect("middle deferred receipt")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        repository
            .schedulable_owner_resume_request_id(middle.id)
            .expect("middle owner resume")
            .is_none()
    );

    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen stopped tree");
    reopened_sqlite
        .migrate()
        .expect("migrate reopened stopped tree");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert!(
        reopened
            .session_repo()
            .schedulable_owner_resume_request_id(middle.id)
            .expect("reopened middle owner resume")
            .is_none()
    );
    reopened
        .session_repo()
        .delete_session_tree(root_session_id)
        .await
        .expect("durable stop fences must cascade with deleted tree");
}

#[tokio::test]
async fn result_before_stop_keeps_forensic_handoff_but_restart_cannot_reseed_resume() {
    let (store, root_session_id) = test_repo().await;
    let owner =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
    let leaf = retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "leaf").await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
    let repository = store.session_repo();

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, None),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("owner early completion"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                leaf.id,
                leaf_admission,
                &completed_terminal_for_response(leaf.id, None),
                leaf_turn,
                None,
                None,
            )
            .await
            .expect("leaf result before Stop"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        repository
            .agent_completion_handoff(leaf.id, leaf_turn)
            .expect("forensic leaf handoff")
            .is_some()
    );
    assert!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("pre-Stop owner resume")
            .is_none()
    );
    assert!(
        repository
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("record result-after-Stop boundary")
            .is_some()
    );
    assert!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("post-Stop owner resume")
            .is_none()
    );

    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen result-first tree");
    reopened_sqlite
        .migrate()
        .expect("migrate reopened result-first tree");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert!(
        reopened
            .session_repo()
            .agent_completion_handoff(leaf.id, leaf_turn)
            .expect("reopened forensic handoff")
            .is_some()
    );
    assert!(
        reopened
            .session_repo()
            .schedulable_owner_resume_request_id(owner.id)
            .expect("reopened owner resume")
            .is_none(),
        "normal completion must not seed an OwnerResume before or after Stop"
    );
}

#[tokio::test]
async fn tree_stop_fence_revokes_old_generation_writes_and_preserves_first_cause() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (old_admission_id, old_turn_id) = active_turn(&store, root_session_id).await;
    let replacement_item_id = store
        .protocol_event_store()
        .list_history_items(root_session_id, old_turn_id)
        .expect("old turn history")
        .into_iter()
        .find(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
        .expect("old user turn")
        .id;
    let protocol_before = (
        store
            .protocol_event_store()
            .list_history_items(root_session_id, old_turn_id)
            .expect("history before fence")
            .len(),
        store
            .protocol_event_store()
            .list_runtime_events(root_session_id, old_turn_id)
            .expect("runtime before fence")
            .len(),
        store
            .protocol_event_store()
            .list_turn_items(root_session_id, old_turn_id)
            .expect("turn items before fence")
            .len(),
    );

    repository
        .record_agent_tree_stop_fence(
            root_session_id,
            crate::protocol::TurnInterruptionCause::UserStop,
        )
        .await
        .expect("record first Stop owner")
        .expect("first Stop fence");
    assert!(matches!(
        repository
            .renew_admitted_run_lease(root_session_id, old_admission_id, old_turn_id)
            .await
            .expect("old lease renewal"),
        RunAdmissionLeaseRenewalOutcome::StopFenced(TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::UserStop
        })
    ));
    assert!(matches!(
        repository
            .admitted_run_state(root_session_id, old_admission_id, old_turn_id)
            .await
            .expect("typed old admission state"),
        AdmittedRunState::StopFenced(TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::UserStop
        })
    ));
    assert_eq!(
        repository
            .admitted_run_status(root_session_id, old_admission_id, old_turn_id)
            .await
            .expect("old admitted status"),
        None
    );
    assert!(
        !repository
            .has_fresh_run_admission(root_session_id)
            .await
            .expect("old effective admission")
    );
    assert!(
        !repository
            .session_blocks_mutation(root_session_id)
            .await
            .expect("old effective mutation owner")
    );
    repository
        .record_model_response_with_protocol_bundle(
            root_session_id,
            old_admission_id,
            old_turn_id,
            ModelResponseWrite {
                response_id: ModelResponseId::new(),
                assistant_text: Some("late response must not commit".to_string()),
                assistant_protocol_sequence_no: None,
                tool_calls: Vec::new(),
            },
        )
        .await
        .expect_err("tree Stop must reject a late model response");
    repository
        .commit_admitted_compaction_with_protocol_bundle(
            root_session_id,
            old_admission_id,
            &RunEvent::CompactionCompleted {
                summarized_messages: 1,
                layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
                clm_checkpoint: None,
                preserved_user_messages: vec!["canonical request".to_string()],
                summary: "late compaction must not commit".to_string(),
                replacement_item_ids: vec![replacement_item_id],
            },
            old_turn_id,
            None,
        )
        .await
        .expect_err("tree Stop must reject a late compaction");
    assert_eq!(
        (
            store
                .protocol_event_store()
                .list_history_items(root_session_id, old_turn_id)
                .expect("history after rejected writes")
                .len(),
            store
                .protocol_event_store()
                .list_runtime_events(root_session_id, old_turn_id)
                .expect("runtime after rejected writes")
                .len(),
            store
                .protocol_event_store()
                .list_turn_items(root_session_id, old_turn_id)
                .expect("turn items after rejected writes")
                .len(),
        ),
        protocol_before
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                old_admission_id,
                &completed_terminal(root_session_id),
                old_turn_id,
                None,
                None,
            )
            .await
            .expect("late success is a typed stale terminal"),
        AdmittedTerminalCommit::NotOwned
    );

    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    repository
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/child",
                    "/root",
                    "append after first fence",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("advance durable append order");
    let later_fence = repository
        .record_agent_tree_stop_fence(
            root_session_id,
            crate::protocol::TurnInterruptionCause::ApprovalAborted,
        )
        .await
        .expect("record overlapping Stop")
        .expect("later overlapping fence");
    let old_target = repository
        .captured_running_terminal_target(root_session_id)
        .await
        .expect("capture overlapping target")
        .expect("old generation remains physically running before fanout");
    assert_eq!(
        repository
            .tree_stop_interruption_cause_for_running_target_at_fence(
                root_session_id,
                old_target,
                later_fence,
            )
            .await
            .expect("derive first Stop cause"),
        Some(crate::protocol::TurnInterruptionCause::UserStop)
    );
    let approval_terminal = RunEvent::TurnTerminal {
        session_id: root_session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::ApprovalAborted,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                old_admission_id,
                &approval_terminal,
                old_turn_id,
                None,
                None,
            )
            .await
            .expect("later Stop cause must not win"),
        AdmittedTerminalCommit::NotOwned
    );
    let user_stop_terminal = RunEvent::TurnTerminal {
        session_id: root_session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                old_admission_id,
                &user_stop_terminal,
                old_turn_id,
                None,
                None,
            )
            .await
            .expect("first Stop cause closes old generation"),
        AdmittedTerminalCommit::Applied
    );

    let (new_admission_id, new_turn_id) = active_turn(&store, root_session_id).await;
    assert!(matches!(
        repository
            .renew_admitted_run_lease(root_session_id, new_admission_id, new_turn_id)
            .await
            .expect("new generation renewal"),
        RunAdmissionLeaseRenewalOutcome::Renewed
    ));
    repository
        .record_model_response_with_protocol_bundle(
            root_session_id,
            new_admission_id,
            new_turn_id,
            ModelResponseWrite {
                response_id: ModelResponseId::new(),
                assistant_text: Some("new generation response".to_string()),
                assistant_protocol_sequence_no: None,
                tool_calls: Vec::new(),
            },
        )
        .await
        .expect("new generation model response");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                new_admission_id,
                &completed_terminal(root_session_id),
                new_turn_id,
                None,
                None,
            )
            .await
            .expect("new generation terminal"),
        AdmittedTerminalCommit::Applied
    );
}

#[tokio::test]
async fn durable_mail_append_revalidates_caller_generation_in_recipient_transaction() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let recipient = retained_test_agent(
        &store,
        root_session_id,
        root_session_id,
        "/root",
        "recipient",
    )
    .await;
    let (old_admission_id, old_turn_id) = active_turn(&store, root_session_id).await;
    let recipient_history_before = store
        .protocol_event_store()
        .list_history_items_for_session(recipient.id)
        .expect("recipient history before Stop")
        .len();
    repository
        .record_agent_tree_stop_fence(
            root_session_id,
            crate::protocol::TurnInterruptionCause::UserStop,
        )
        .await
        .expect("caller Stop fence")
        .expect("caller generation boundary");

    for trigger_turn in [false, true] {
        repository
            .append_inter_agent_communication_for_caller_turn_with_protocol_bundle_and_capacity(
                root_session_id,
                old_admission_id,
                old_turn_id,
                recipient.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/recipient".to_string(),
                    content: render_inter_agent_message(
                        if trigger_turn {
                            InterAgentMessageType::NewTask
                        } else {
                            InterAgentMessageType::Message
                        },
                        "/root/recipient",
                        "/root",
                        "stale caller mail",
                    ),
                    trigger_turn,
                },
                false,
                true,
            )
            .expect_err("pre-fence caller must not append mail after Stop");
    }
    assert_eq!(
        store
            .protocol_event_store()
            .list_history_items_for_session(recipient.id)
            .expect("recipient history after rejected mail")
            .len(),
        recipient_history_before
    );

    let user_stop_terminal = RunEvent::TurnTerminal {
        session_id: root_session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                old_admission_id,
                &user_stop_terminal,
                old_turn_id,
                None,
                None,
            )
            .await
            .expect("close old caller generation"),
        AdmittedTerminalCommit::Applied
    );
    let (new_admission_id, new_turn_id) = active_turn(&store, root_session_id).await;
    let stored = repository
        .append_inter_agent_communication_for_caller_turn_with_protocol_bundle_and_capacity(
            root_session_id,
            new_admission_id,
            new_turn_id,
            recipient.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/recipient".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/recipient",
                    "/root",
                    "new generation followup",
                ),
                trigger_turn: true,
            },
            false,
            true,
        )
        .expect("post-fence caller mail");
    assert!(stored.schedule_turn);
    assert_eq!(
        store
            .protocol_event_store()
            .list_history_items_for_session(recipient.id)
            .expect("recipient history after valid mail")
            .len(),
        recipient_history_before
    );
    assert_eq!(
        repository
            .agent_mailbox_communications_by_id(recipient.id, &[stored.history_item_id])
            .expect("recipient pending mailbox")
            .len(),
        1
    );
}

#[tokio::test]
async fn observed_running_turn_creates_then_reuses_service_stop_fence_without_extending_boundary() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(&store, root_session_id).await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (_child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    for invalid_turn_id in [TurnId::new(), child_turn_id] {
        repository
            .record_agent_tree_stop_fence_for_observed_turn(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
                invalid_turn_id,
            )
            .await
            .expect_err("foreign or nonexistent observed turn must fail closed");
    }
    assert_eq!(
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("fence count after invalid handles"),
        0
    );
    let user_stop_terminal = RunEvent::TurnTerminal {
        session_id: root_session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                admission_id,
                &user_stop_terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("worker Stop terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("fence count after exact worker Stop"),
        0,
        "an ordinary exact worker Stop must not create a subtree fence"
    );
    let first_fence = repository
        .record_agent_tree_stop_fence_for_observed_turn(
            root_session_id,
            crate::protocol::TurnInterruptionCause::UserStop,
            turn_id,
        )
        .await
        .expect("service creates observed-turn fence")
        .expect("observed turn owns a tree-Stop boundary");
    assert_eq!(
        repository
            .record_agent_tree_stop_fence_for_observed_turn(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
                turn_id,
            )
            .await
            .expect("service reuses observed-turn fence"),
        Some(first_fence)
    );
    let count_after_observed_reuse = repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT COUNT(*) FROM agent_tree_stop_fences WHERE stopped_session_id = ?1",
            params![root_session_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .expect("fence count after observed reuse");
    assert_eq!(count_after_observed_reuse, 1);
}

#[tokio::test]
async fn legacy_completed_early_generation_is_discarded_by_a_later_fence() {
    let (store, root_session_id) = test_repo().await;
    let parent =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "parent").await;
    let owner =
        retained_test_agent(&store, root_session_id, parent.id, "/root/parent", "owner").await;
    let leaf = retained_test_agent(
        &store,
        root_session_id,
        owner.id,
        "/root/parent/owner",
        "leaf",
    )
    .await;
    let (parent_admission, parent_turn) = active_turn(&store, parent.id).await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;
    let repository = store.session_repo();

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                parent.id,
                parent_admission,
                &agent_interrupted_terminal(parent.id),
                parent_turn,
                None,
                None,
            )
            .await
            .expect("cancel reusable parent"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, None),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("pre-fence CompletedEarly"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(&store, owner.id, owner_turn, parent.id);
    assert!(
        repository
            .pending_deferred_completion(owner.id)
            .expect("pre-fence effective deferred")
            .is_some()
    );
    let fence = repository
        .record_agent_tree_stop_fence(
            root_session_id,
            crate::protocol::TurnInterruptionCause::UserStop,
        )
        .await
        .expect("record tree Stop")
        .expect("stored tree Stop fence");
    assert!(
        repository
            .pending_deferred_completion(owner.id)
            .expect("post-fence effective deferred")
            .is_none(),
        "a committed fence must hide stale pending work before fanout settles"
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, owner_turn)
            .expect("physical fence effects")
            .deferred
            .expect("physical pre-fence deferred receipt")
            .state,
        DeferredAgentCompletionState::Discarded,
        "the fence must free the unique pending slot, not only hide it through a view"
    );

    let new_leaf = retained_test_agent(
        &store,
        root_session_id,
        owner.id,
        "/root/parent/owner",
        "new_leaf",
    )
    .await;
    let (new_leaf_admission, new_leaf_turn) = active_turn(&store, new_leaf.id).await;
    let (new_owner_admission, new_owner_turn) = active_turn(&store, owner.id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                new_owner_admission,
                &completed_terminal_for_response(owner.id, None),
                new_owner_turn,
                None,
                None,
            )
            .await
            .expect("post-fence CompletedEarly"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(
        &store,
        owner.id,
        new_owner_turn,
        parent.id,
    );
    assert_eq!(
        repository
            .pending_deferred_completion(owner.id)
            .expect("post-fence pending deferred")
            .expect("new generation owns pending slot")
            .agent_turn_id,
        new_owner_turn
    );

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                leaf.id,
                leaf_admission,
                &agent_interrupted_terminal(leaf.id),
                leaf_turn,
                None,
                None,
            )
            .await
            .expect("late old worker interruption"),
        AdmittedTerminalCommit::NotOwned
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                leaf.id,
                leaf_admission,
                &tree_stopped_terminal(leaf.id),
                leaf_turn,
                None,
                None,
            )
            .await
            .expect("compatible post-fence descendant settlement"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .pending_deferred_completion(owner.id)
            .expect("new pending after old terminal")
            .expect("late old terminal must not discard new generation")
            .agent_turn_id,
        new_owner_turn
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, owner_turn)
            .expect("fenced owner effects")
            .deferred
            .expect("physical deferred receipt")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        repository
            .agent_completion_handoff(owner.id, owner_turn)
            .expect("fenced owner handoff")
            .is_none()
    );
    assert!(
        repository
            .pending_agent_trigger_history_item_id_for_tree_stop(leaf.id, fence)
            .expect("exact Stop lookup")
            .is_none()
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                new_leaf.id,
                new_leaf_admission,
                &completed_terminal_for_response(new_leaf.id, None),
                new_leaf_turn,
                None,
                None,
            )
            .await
            .expect("post-fence child result"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, new_owner_turn)
            .expect("new generation owner effects")
            .deferred
            .expect("new generation deferred receipt")
            .state,
        DeferredAgentCompletionState::Superseded
    );
}

#[tokio::test]
async fn synthetic_tree_stop_does_not_reclaim_a_terminalized_explicit_wake() {
    let (store, root_session_id) = test_repo().await;
    let (child, trigger_history_item_id, _) =
        spawn_pending_child(&store, root_session_id, "claimed_tree_stop").await;
    let repository = store.session_repo();
    let child_turn_id = TurnId::new();
    let admission = repository
        .admit_agent_triggered_turn(child.id, child_turn_id, trigger_history_item_id)
        .await
        .expect("explicit wake admission")
        .expect("explicit wake admitted");
    let fence = repository
        .record_agent_tree_stop_fence(
            root_session_id,
            crate::protocol::TurnInterruptionCause::UserStop,
        )
        .await
        .expect("record root tree Stop")
        .expect("root tree-stop fence");
    assert_eq!(
        repository
            .pending_agent_trigger_history_item_id_for_tree_stop(child.id, fence)
            .expect("fenced explicit wake"),
        Some(trigger_history_item_id)
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                admission.admission_id,
                &tree_stopped_terminal(child.id),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize claimed tree-stopped wake"),
        AdmittedTerminalCommit::Applied
    );

    let claim_before = {
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                         FROM agent_trigger_turn_claims
                         WHERE history_item_id = ?1",
                    [trigger_history_item_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("exact wake claim count"),
            1
        );
        agent_trigger_turn_claim_in_connection(&connection, child.id, trigger_history_item_id)
            .expect("exact wake claim")
            .expect("claimed wake owner")
    };
    assert_eq!(claim_before, (admission.admission_id, child_turn_id));
    let terminal_before = repository
        .durable_terminal_for_turn(child.id, child_turn_id)
        .await
        .expect("claimed turn terminal")
        .expect("claimed turn is terminal");
    assert!(matches!(
        terminal_before.outcome,
        TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::TreeStopped
        }
    ));
    let events_before = store
        .protocol_event_store()
        .list_runtime_events_for_session(child.id)
        .expect("claimed turn events");
    let started_before = events_before
        .iter()
        .filter(|event| {
            event.turn_id == child_turn_id
                && matches!(
                    &event.msg,
                    RuntimeEventMsg::Warning { message }
                        if message.starts_with("thread started:")
                )
        })
        .map(|event| event.id)
        .collect::<Vec<_>>();
    let terminals_before = events_before
        .iter()
        .filter(|event| {
            event.turn_id == child_turn_id
                && matches!(&event.msg, RuntimeEventMsg::TurnTerminal { .. })
        })
        .map(|event| event.id)
        .collect::<Vec<_>>();
    assert_eq!(started_before.len(), 1);
    assert_eq!(terminals_before.len(), 1);
    assert_eq!(events_before.len(), 2);
    assert!(
        repository
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("claimed turn handoff")
            .is_none()
    );

    assert!(matches!(
        repository
            .settle_pending_agent_trigger_at_tree_stop_fence(
                child.id,
                trigger_history_item_id,
                fence,
            )
            .expect("idempotent synthetic tree-stop settlement"),
        PendingAgentTriggerSettlement::WakeOwnedOrResolved
    ));

    let claim_after = {
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                         FROM agent_trigger_turn_claims
                         WHERE history_item_id = ?1",
                    [trigger_history_item_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("retained exact wake claim count"),
            1
        );
        agent_trigger_turn_claim_in_connection(&connection, child.id, trigger_history_item_id)
            .expect("retained exact wake claim")
            .expect("retained claimed wake owner")
    };
    assert_eq!(claim_after, claim_before);
    assert!(matches!(
        repository
            .durable_terminal_for_turn(child.id, child_turn_id)
            .await
            .expect("retained claimed turn terminal")
            .expect("retained claimed turn remains terminal")
            .outcome,
        TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::TreeStopped
        }
    ));
    let events_after = store
        .protocol_event_store()
        .list_runtime_events_for_session(child.id)
        .expect("events after idempotent tree Stop");
    assert_eq!(
        events_after
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        events_before
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        "synthetic settlement must not append a second SessionStarted or terminal"
    );
    assert!(
        events_after
            .iter()
            .all(|event| event.turn_id == child_turn_id)
    );
    assert_eq!(
        repository
            .get_session(child.id)
            .await
            .expect("terminal child session")
            .status,
        SessionStatus::Cancelled
    );
    assert!(
        repository
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("retained claimed turn handoff")
            .is_none()
    );
}

#[tokio::test]
async fn synthetic_stop_settlement_uses_first_fence_and_rejects_out_of_boundary_trigger() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let repository = store.session_repo();
    let old = repository
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/child",
                    "/root",
                    "old trigger",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("old trigger");
    let first_fence = repository
        .record_agent_tree_stop_fence(child.id, crate::protocol::TurnInterruptionCause::UserStop)
        .await
        .expect("first child Stop")
        .expect("first child fence");
    let new = repository
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/child",
                    "/root",
                    "post-first-fence trigger",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("new trigger");
    let _later_fence = repository
        .record_agent_tree_stop_fence(
            child.id,
            crate::protocol::TurnInterruptionCause::ApprovalAborted,
        )
        .await
        .expect("later child Stop")
        .expect("later child fence");

    assert!(matches!(
        repository
            .settle_pending_agent_trigger_at_tree_stop_fence(
                child.id,
                new.history_item_id,
                first_fence,
            )
            .expect("out-of-boundary settlement"),
        PendingAgentTriggerSettlement::WakeOwnedOrResolved
    ));
    assert!(matches!(
        repository
            .settle_pending_agent_trigger_with_terminal(
                child.id,
                old.history_item_id,
                DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Interrupted {
                        cause: crate::protocol::TurnInterruptionCause::UserStop,
                    },
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                },
            )
            .expect("generic fenced settlement"),
        PendingAgentTriggerSettlement::WakeOwnedOrResolved
    ));
    let PendingAgentTriggerSettlement::Applied {
        turn_id: settlement_turn_id,
        ..
    } = repository
        .settle_pending_agent_trigger_at_tree_stop_fence(child.id, old.history_item_id, first_fence)
        .expect("exact fenced settlement")
    else {
        panic!("exact fenced settlement must claim the old trigger");
    };
    assert!(matches!(
        repository
            .durable_terminal_for_turn(child.id, settlement_turn_id)
            .await
            .expect("synthetic terminal")
            .expect("synthetic durable terminal")
            .outcome,
        TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::UserStop
        }
    ));
    let fence_count = repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT COUNT(*)
                 FROM agent_tree_stop_fences
                 WHERE stopped_session_id = ?1",
            params![child.id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .expect("child fence count");
    assert_eq!(
        fence_count, 2,
        "synthetic settlement must reuse the first fence rather than creating a third"
    );
}

#[tokio::test]
async fn fence_hides_old_trigger_from_restart_context_but_allows_new_explicit_followup() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let repository = store.session_repo();
    let old = repository
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/child",
                    "/root",
                    "old task must not replay",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("old pending trigger");
    let fence = repository
        .record_agent_tree_stop_fence(
            root_session_id,
            crate::protocol::TurnInterruptionCause::UserStop,
        )
        .await
        .expect("record context fence")
        .expect("stored context fence");
    assert_eq!(
        repository
            .pending_agent_trigger_history_item_id(child.id)
            .expect("normal old trigger lookup"),
        None
    );
    assert_eq!(
        repository
            .pending_agent_trigger_history_item_id_for_tree_stop(child.id, fence)
            .expect("Stop old trigger lookup"),
        Some(old.history_item_id)
    );

    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen fenced context");
    reopened_sqlite.migrate().expect("migrate reopened context");
    let reopened = StoreBundle::new(reopened_sqlite);
    let mut active_before_followup = Vec::new();
    reopened
        .protocol_event_store()
        .visit_active_history_pages_for_session(
            child.id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |page| {
                active_before_followup.extend(page.items);
                Ok(())
            },
        )
        .expect("fenced active context");
    assert!(
        active_before_followup
            .iter()
            .all(|item| item.id != old.history_item_id)
    );

    let followup = reopened
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/child",
                    "/root",
                    "new explicit followup",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("post-fence explicit followup");
    assert_eq!(
        reopened
            .session_repo()
            .pending_agent_trigger_history_item_id_for_tree_stop(child.id, fence)
            .expect("exact old boundary after followup"),
        Some(old.history_item_id),
        "a later followup must not replace the trigger selected for an earlier Stop"
    );
    assert_eq!(
        reopened
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("new trigger lookup"),
        Some(followup.history_item_id)
    );
    let followup_turn = TurnId::new();
    let admitted = reopened
        .session_repo()
        .admit_agent_triggered_turn(child.id, followup_turn, followup.history_item_id)
        .await
        .expect("followup admission")
        .expect("post-fence followup admitted");
    assert_eq!(
        reopened
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                child.id,
                admitted.admission_id,
                followup_turn,
                128,
            )
            .expect("safe post-fence followup delivery")
            .history_item_ids,
        vec![followup.history_item_id]
    );
    let mut followup_context_items = Vec::new();
    reopened
        .protocol_event_store()
        .visit_active_history_pages_for_session(
            child.id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |page| {
                followup_context_items.extend(page.items);
                Ok(())
            },
        )
        .expect("post-fence model context");
    let model_messages =
        crate::agent::context_manager::ContextManager::rehydrate(followup_context_items)
            .model_messages(false);
    assert!(model_messages.iter().any(|message| matches!(
        message,
        crate::llm::ModelMessage::Agent { content }
            if content.contains("new explicit followup")
    )));
    assert!(model_messages.iter().all(|message| !matches!(
        message,
        crate::llm::ModelMessage::Agent { content }
            if content.contains("old task must not replay")
    )));
    assert_eq!(
        reopened
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                admitted.admission_id,
                &completed_terminal_for_response(child.id, None),
                followup_turn,
                None,
                None,
            )
            .await
            .expect("post-fence followup terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        reopened
            .session_repo()
            .agent_completion_handoff(child.id, followup_turn)
            .expect("post-fence followup handoff")
            .is_some()
    );
}

#[tokio::test]
async fn legacy_completed_early_releases_once_after_last_agent_interruption() {
    let (store, root_session_id) = test_repo().await;
    let owner =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
    let child =
        retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (child_admission, child_turn) = active_turn(&store, child.id).await;

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, None),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("owner early completion"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(
        &store,
        owner.id,
        owner_turn,
        root_session_id,
    );
    assert!(
        store
            .session_repo()
            .agent_completion_handoff(owner.id, owner_turn)
            .expect("early owner handoff")
            .is_none()
    );
    assert_eq!(
        store
            .session_repo()
            .pending_deferred_completion(owner.id)
            .expect("pending owner deferred")
            .expect("owner deferred")
            .kind,
        DeferredAgentCompletionKind::CompletedEarly
    );

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission,
                &agent_interrupted_terminal(child.id),
                child_turn,
                None,
                None,
            )
            .await
            .expect("child interruption"),
        AdmittedTerminalCommit::Applied
    );
    let effects = store
        .session_repo()
        .agent_terminal_effects(child.id, child_turn)
        .expect("interruption effects");
    assert_eq!(effects.released_deferred_handoffs.len(), 1);
    assert_eq!(
        effects.released_deferred_handoffs[0].child_session_id,
        owner.id
    );
    assert_eq!(
        store
            .session_repo()
            .agent_terminal_effects(owner.id, owner_turn)
            .expect("owner deferred effects")
            .deferred
            .expect("owner deferred receipt")
            .state,
        DeferredAgentCompletionState::Released
    );
    assert!(
        store
            .session_repo()
            .pending_deferred_completion(owner.id)
            .expect("resolved owner deferred")
            .is_none()
    );
}

#[tokio::test]
async fn legacy_completed_early_snapshot_keeps_release_and_trigger_readiness_atomic() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let owner =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
    let child =
        retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (child_admission, child_turn) = active_turn(&store, child.id).await;
    let owner_response = record_text_response(
        &store,
        owner.id,
        owner_admission,
        owner_turn,
        "owner result",
    )
    .await;

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, Some(owner_response)),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("owner completed early"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(
        &store,
        owner.id,
        owner_turn,
        root_session_id,
    );
    let explicit = repository
        .append_inter_agent_communication_with_protocol_bundle(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "queued while owner awaits child",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("deferred explicit trigger");
    assert!(!explicit.schedule_turn);
    let snapshot_limit = crate::runtime::agent_control::MAX_RETAINED_AGENTS.saturating_sub(1);
    let before = store
        .protocol_event_store()
        .retained_descendant_snapshot(root_session_id, snapshot_limit)
        .expect("pre-release retained snapshot");
    let before_owner = before
        .iter()
        .find(|item| item.edge.child_session_id == owner.id)
        .expect("pre-release owner");
    assert_eq!(before_owner.pending_deferred_turn_id, Some(owner_turn));
    assert_eq!(
        before_owner.pending_deferred_completion_kind,
        Some(DeferredAgentCompletionKind::CompletedEarly)
    );
    assert_eq!(
        before_owner.pending_trigger_history_item_id,
        Some(explicit.history_item_id)
    );
    assert!(!before_owner.pending_trigger_schedule_ready);
    assert_eq!(before_owner.pending_owner_resume_request_id, None);
    assert_eq!(before_owner.session_status, "completed");
    assert!(matches!(
        before_owner.latest_task_content.as_deref(),
        Some([ContentPart::Text { text }]) if text == "canonical request"
    ));
    assert!(matches!(
        before_owner.latest_assistant_content.as_deref(),
        Some([ContentPart::Text { text }]) if text == "owner result"
    ));
    assert_eq!(before_owner.latest_error, None);
    assert_eq!(before_owner.interruption_cause, None);

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission,
                &completed_terminal_for_response(child.id, None),
                child_turn,
                None,
                None,
            )
            .await
            .expect("child terminal releases owner"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = repository
        .agent_completion_handoff(child.id, child_turn)
        .expect("child handoff")
        .expect("stored child handoff");
    assert_eq!(handoff.released_owner_deferred_turn_id, Some(owner_turn));
    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen released retained tree");
    reopened_sqlite
        .migrate()
        .expect("migrate reopened retained tree");
    let reopened = StoreBundle::new(reopened_sqlite);
    let after = reopened
        .protocol_event_store()
        .retained_descendant_snapshot(root_session_id, snapshot_limit)
        .expect("post-release retained snapshot");
    let after_owner = after
        .iter()
        .find(|item| item.edge.child_session_id == owner.id)
        .expect("post-release owner");
    assert_eq!(after_owner.pending_deferred_turn_id, None);
    assert_eq!(after_owner.pending_deferred_completion_kind, None);
    assert_eq!(
        after_owner.pending_trigger_history_item_id,
        Some(explicit.history_item_id)
    );
    assert!(after_owner.pending_trigger_schedule_ready);
    let raw_owner_resume = repository
        .list_pending_owner_resume_requests(owner.id)
        .expect("raw owner resume after child completion");
    assert_eq!(raw_owner_resume.len(), 1);
    assert_eq!(
        after_owner.pending_owner_resume_request_id,
        Some(raw_owner_resume[0].request_id),
        "the snapshot intentionally carries raw scheduler ownership; explicit admission coalesces it"
    );
    assert_eq!(after_owner.session_status, "completed");
    assert!(matches!(
        after_owner.latest_task_content.as_deref(),
        Some([ContentPart::Text { text }]) if text == "canonical request"
    ));
    assert!(matches!(
        after_owner.latest_assistant_content.as_deref(),
        Some([ContentPart::Text { text }]) if text == "owner result"
    ));
    assert_eq!(after_owner.latest_error, None);
    assert_eq!(after_owner.interruption_cause, None);
}

#[tokio::test]
async fn retained_snapshot_pages_over_two_hundred_rows_in_one_read_transaction() {
    let (store, root_session_id) = test_repo().await;
    let descendant_count = crate::protocol::MAX_PROTOCOL_PAGE_LIMIT + 1;
    let mut descendants = Vec::with_capacity(descendant_count);
    for index in 0..descendant_count {
        descendants.push(
            retained_test_agent(
                &store,
                root_session_id,
                root_session_id,
                "/root",
                &format!("paged_{index:03}"),
            )
            .await,
        );
    }
    let second_page_child = descendants[crate::protocol::MAX_PROTOCOL_PAGE_LIMIT].id;
    let reopened_sqlite =
        SqliteStore::open(store.paths()).expect("open independent snapshot writer");
    reopened_sqlite
        .migrate()
        .expect("migrate independent snapshot writer");
    let competing_store = StoreBundle::new(reopened_sqlite);
    let mut observed_page_ends = Vec::new();
    let mut mutated_between_pages = false;
    let mut page_boundary_trigger_id = None;

    let snapshot = store
        .protocol_event_store()
        .retained_descendant_snapshot_observing_pages(
            root_session_id,
            descendant_count,
            &mut |read_count| {
                observed_page_ends.push(read_count);
                if read_count == crate::protocol::MAX_PROTOCOL_PAGE_LIMIT {
                    let appended = competing_store
                        .session_repo()
                        .append_inter_agent_communication_with_protocol_bundle(
                            second_page_child,
                            InterAgentCommunication {
                                author: "/root".to_string(),
                                recipient: format!(
                                    "/root/paged_{:03}",
                                    crate::protocol::MAX_PROTOCOL_PAGE_LIMIT
                                ),
                                content: render_inter_agent_message(
                                    InterAgentMessageType::NewTask,
                                    &format!(
                                        "/root/paged_{:03}",
                                        crate::protocol::MAX_PROTOCOL_PAGE_LIMIT
                                    ),
                                    "/root",
                                    "valid page-boundary followup",
                                ),
                                trigger_turn: true,
                            },
                            false,
                        )?;
                    if !appended.schedule_turn {
                        return Err(StorageError::Message(
                            "page-boundary followup was not a schedulable first trigger"
                                .to_string(),
                        ));
                    }
                    page_boundary_trigger_id = Some(appended.history_item_id);
                    mutated_between_pages = true;
                }
                Ok(())
            },
        )
        .expect("multi-page retained snapshot");

    assert!(mutated_between_pages);
    assert_eq!(
        observed_page_ends,
        vec![crate::protocol::MAX_PROTOCOL_PAGE_LIMIT, descendant_count]
    );
    assert_eq!(snapshot.len(), descendant_count);
    assert_eq!(
        snapshot
            .iter()
            .find(|item| item.edge.child_session_id == second_page_child)
            .expect("second-page child in stable snapshot")
            .pending_trigger_history_item_id,
        None,
        "the second page must retain the trigger state established by the first-page read snapshot"
    );
    let exact_boundary_trigger =
        page_boundary_trigger_id.expect("valid page-boundary followup identity");
    assert_eq!(
        store
            .protocol_event_store()
            .retained_descendant_page(root_session_id, crate::protocol::MAX_PROTOCOL_PAGE_LIMIT, 1,)
            .expect("fresh page after snapshot")
            .items[0]
            .pending_trigger_history_item_id,
        Some(exact_boundary_trigger),
        "a later transaction must observe the committed page-boundary mutation"
    );
    assert!(
        store
            .protocol_event_store()
            .retained_descendant_page(root_session_id, crate::protocol::MAX_PROTOCOL_PAGE_LIMIT, 1,)
            .expect("fresh readiness page after snapshot")
            .items[0]
            .pending_trigger_schedule_ready
    );

    let zero_error = store
        .protocol_event_store()
        .retained_descendant_snapshot(root_session_id, 0)
        .expect_err("zero snapshot bound must be rejected");
    assert!(zero_error.to_string().contains("must be at least 1"));
    let bounded_error = store
        .protocol_event_store()
        .retained_descendant_snapshot(root_session_id, crate::protocol::MAX_PROTOCOL_PAGE_LIMIT)
        .expect_err("a custom bound below the retained count must reject truncation");
    assert!(
        bounded_error
            .to_string()
            .contains("exceeding the supported maximum 200")
    );
}

#[tokio::test]
async fn tree_stop_discards_legacy_completed_early_without_releasing_final() {
    let (store, root_session_id) = test_repo().await;
    let owner =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "owner").await;
    let child =
        retained_test_agent(&store, root_session_id, owner.id, "/root/owner", "child").await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (child_admission, child_turn) = active_turn(&store, child.id).await;
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, None),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("owner early completion"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(
        &store,
        owner.id,
        owner_turn,
        root_session_id,
    );
    assert!(
        store
            .session_repo()
            .record_agent_tree_stop_fence(
                root_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("record explicit root tree-Stop boundary")
            .is_some()
    );
    let stopped = RunEvent::TurnTerminal {
        session_id: child.id,
        terminal: Box::new(pre_admission_interrupted_terminal()),
    };
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission,
                &stopped,
                child_turn,
                None,
                None,
            )
            .await
            .expect("tree-stopped child"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        store
            .session_repo()
            .agent_terminal_effects(owner.id, owner_turn)
            .expect("discarded owner deferred")
            .deferred
            .expect("owner deferred receipt")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        store
            .session_repo()
            .agent_completion_handoff(owner.id, owner_turn)
            .expect("discarded owner handoff")
            .is_none()
    );
    assert!(
        store
            .session_repo()
            .agent_terminal_effects(child.id, child_turn)
            .expect("tree-stop effects")
            .released_deferred_handoffs
            .is_empty()
    );
    let project_id = store
        .session_repo()
        .get_session(root_session_id)
        .await
        .expect("root before subtree delete")
        .project_id;
    store
        .session_repo()
        .delete_session_tree(root_session_id)
        .await
        .expect("delete subtree with discarded deferred receipt");
    store
        .project_repo()
        .delete_project(project_id)
        .await
        .expect("delete project after deferred subtree cleanup");
}

#[tokio::test]
async fn nested_legacy_completed_early_release_supersedes_outer_candidate() {
    let (store, root_session_id) = test_repo().await;
    let outer =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "outer").await;
    let middle =
        retained_test_agent(&store, root_session_id, outer.id, "/root/outer", "middle").await;
    let leaf = retained_test_agent(
        &store,
        root_session_id,
        middle.id,
        "/root/outer/middle",
        "leaf",
    )
    .await;
    let (outer_admission, outer_turn) = active_turn(&store, outer.id).await;
    let (middle_admission, middle_turn) = active_turn(&store, middle.id).await;
    let (leaf_admission, leaf_turn) = active_turn(&store, leaf.id).await;

    for (session_id, admission_id, turn_id, parent_session_id) in [
        (middle.id, middle_admission, middle_turn, outer.id),
        (outer.id, outer_admission, outer_turn, root_session_id),
    ] {
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
                    &completed_terminal_for_response(session_id, None),
                    turn_id,
                    None,
                    None,
                )
                .await
                .expect("nested early completion"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            session_id,
            turn_id,
            parent_session_id,
        );
    }

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                leaf.id,
                leaf_admission,
                &agent_interrupted_terminal(leaf.id),
                leaf_turn,
                None,
                None,
            )
            .await
            .expect("leaf interruption"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        store
            .session_repo()
            .agent_terminal_effects(middle.id, middle_turn)
            .expect("middle deferred")
            .deferred
            .expect("middle deferred receipt")
            .state,
        DeferredAgentCompletionState::Released
    );
    assert_eq!(
        store
            .session_repo()
            .agent_terminal_effects(outer.id, outer_turn)
            .expect("outer deferred")
            .deferred
            .expect("outer deferred receipt")
            .state,
        DeferredAgentCompletionState::Superseded
    );
    assert!(
        store
            .session_repo()
            .agent_completion_handoff(outer.id, outer_turn)
            .expect("outer handoff")
            .is_none(),
        "the outer owner must resume instead of leaking its early FINAL"
    );
    assert!(
        store
            .session_repo()
            .schedulable_owner_resume_request_id(outer.id)
            .expect("outer OwnerResume")
            .is_some()
    );
}

#[tokio::test]
async fn expired_claimed_owner_resume_recovers_without_leaking_crash_final() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let crashed_turn = TurnId::new();
    let crashed_admission = repository
        .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
        .await
        .expect("initial OwnerResume admission")
        .expect("initial OwnerResume admitted");
    assert_eq!(
        repository
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                crashed_admission.admission_id,
                crashed_turn,
                128,
            )
            .expect("safe claimed OwnerResume delivery")
            .history_item_ids,
        vec![requests[0].request_id.0]
    );
    repository
        .inject_raw_runtime_state_for_corruption_test(
            owner.id,
            "running",
            Some(&crashed_admission.admission_id.to_string()),
            Some(&crashed_turn.to_string()),
            Some(1),
        )
        .expect("expire claimed OwnerResume");

    let recovered_request = repository
        .schedulable_owner_resume_request_id(owner.id)
        .expect("recovered request lookup")
        .unwrap_or(requests[0].request_id);
    let continuation_turn = TurnId::new();
    let continuation_admission = repository
        .admit_owner_resume_turn(owner.id, continuation_turn, recovered_request)
        .await
        .expect("replacement OwnerResume admission")
        .expect("replacement OwnerResume admitted");
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("crash deferred effects")
            .deferred
            .expect("crash deferred receipt")
            .state,
        DeferredAgentCompletionState::Pending
    );
    assert!(
        repository
            .agent_completion_handoff(owner.id, crashed_turn)
            .expect("crash handoff lookup")
            .is_none(),
        "recoverable crash failure must not escape upward"
    );

    repository
        .inject_raw_runtime_state_for_corruption_test(
            owner.id,
            "running",
            Some(&continuation_admission.admission_id.to_string()),
            Some(&continuation_turn.to_string()),
            Some(1),
        )
        .expect("expire replacement OwnerResume");
    let final_turn = TurnId::new();
    let final_admission = repository
        .admit_owner_resume_turn(owner.id, final_turn, recovered_request)
        .await
        .expect("second replacement OwnerResume admission")
        .expect("second replacement OwnerResume admitted");
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("first crash resolution")
            .deferred
            .expect("first crash deferred")
            .state,
        DeferredAgentCompletionState::Superseded
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, continuation_turn)
            .expect("second crash pending")
            .deferred
            .expect("second crash deferred")
            .state,
        DeferredAgentCompletionState::Pending
    );

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                final_admission.admission_id,
                &completed_terminal_for_response(owner.id, None),
                final_turn,
                None,
                None,
            )
            .await
            .expect("replacement OwnerResume terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        repository
            .agent_completion_handoff(owner.id, final_turn)
            .expect("replacement handoff")
            .is_some()
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, continuation_turn)
            .expect("resolved crash deferred effects")
            .deferred
            .expect("resolved crash deferred receipt")
            .state,
        DeferredAgentCompletionState::Superseded
    );
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM agent_completion_handoffs
                     WHERE child_session_id = ?1",
                params![owner.id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("owner upstream handoff count"),
        1
    );
}

#[tokio::test]
async fn admitted_replacement_owner_resume_interruption_discards_crash_deferred() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let crashed_turn = TurnId::new();
    let crashed_admission = repository
        .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
        .await
        .expect("initial OwnerResume admission")
        .expect("initial OwnerResume admitted");
    assert_eq!(
        repository
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                crashed_admission.admission_id,
                crashed_turn,
                128,
            )
            .expect("safe child-result delivery")
            .history_item_ids,
        vec![requests[0].request_id.0]
    );
    let target = repository
        .captured_running_terminal_target(owner.id)
        .await
        .expect("capture OwnerResume")
        .expect("running OwnerResume");
    assert!(
        repository
            .recover_captured_running_session_with_protocol_event(
                owner.id,
                &failed_terminal(owner.id, "owner process crashed"),
                target,
            )
            .await
            .expect("recover crashed OwnerResume")
    );

    let replacement_turn = TurnId::new();
    let replacement = repository
        .admit_owner_resume_turn(owner.id, replacement_turn, requests[0].request_id)
        .await
        .expect("replacement OwnerResume admission")
        .expect("replacement OwnerResume admitted");
    let interrupted = RunEvent::TurnTerminal {
        session_id: owner.id,
        terminal: Box::new(pre_admission_agent_interrupted_terminal()),
    };
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                replacement.admission_id,
                &interrupted,
                replacement_turn,
                None,
                None,
            )
            .await
            .expect("interrupt replacement OwnerResume"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("discarded crash effects")
            .deferred
            .expect("crash deferred receipt")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        repository
            .agent_completion_handoff(owner.id, replacement_turn)
            .expect("interrupted replacement handoff")
            .is_none(),
        "an interrupted OwnerResume must not escape upward"
    );
    assert!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("resolved OwnerResume")
            .is_none()
    );

    let project_id = repository
        .get_session(root_session_id)
        .await
        .expect("root before cleanup")
        .project_id;
    repository
        .delete_session_tree(root_session_id)
        .await
        .expect("discarded crash deferred must not block tree deletion");
    store
        .project_repo()
        .delete_project(project_id)
        .await
        .expect("discarded crash deferred must not block project deletion");
}

#[tokio::test]
async fn explicit_trigger_recovers_pending_crash_owner_resume() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let crashed_turn = TurnId::new();
    let crashed_admission = repository
        .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
        .await
        .expect("initial OwnerResume admission")
        .expect("initial OwnerResume admitted");
    assert_eq!(
        repository
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                crashed_admission.admission_id,
                crashed_turn,
                128,
            )
            .expect("safe child-result delivery")
            .history_item_ids,
        vec![requests[0].request_id.0]
    );
    let target = repository
        .captured_running_terminal_target(owner.id)
        .await
        .expect("capture OwnerResume")
        .expect("running OwnerResume");
    assert!(
        repository
            .recover_captured_running_session_with_protocol_event(
                owner.id,
                &failed_terminal(owner.id, "owner process crashed"),
                target,
            )
            .await
            .expect("recover crashed OwnerResume")
    );
    let explicit = repository
        .append_inter_agent_communication_with_protocol_bundle(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "explicit recovery work",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("explicit recovery trigger");
    assert!(explicit.schedule_turn);
    let projection = store
        .protocol_event_store()
        .retained_descendant_page(root_session_id, 0, 10)
        .expect("explicit crash recovery projection");
    let projected_owner = projection
        .items
        .iter()
        .find(|item| item.edge.child_session_id == owner.id)
        .expect("projected crash owner");
    assert_eq!(
        projected_owner.pending_trigger_history_item_id,
        Some(explicit.history_item_id)
    );
    assert!(projected_owner.pending_trigger_schedule_ready);
    assert_eq!(
        projected_owner.pending_owner_resume_request_id,
        Some(requests[0].request_id)
    );
    let recovery_turn = TurnId::new();
    let recovery = repository
        .admit_agent_triggered_turn(owner.id, recovery_turn, explicit.history_item_id)
        .await
        .expect("explicit recovery admission")
        .expect("explicit recovery admitted");
    assert_eq!(
        repository
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                recovery.admission_id,
                recovery_turn,
                128,
            )
            .expect("safe explicit-recovery delivery")
            .history_item_ids,
        vec![explicit.history_item_id]
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                recovery.admission_id,
                &completed_terminal_for_response(owner.id, None),
                recovery_turn,
                None,
                None,
            )
            .await
            .expect("explicit recovery terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("superseded crash effects")
            .deferred
            .expect("crash deferred receipt")
            .state,
        DeferredAgentCompletionState::Superseded
    );
    assert!(
        repository
            .agent_completion_handoff(owner.id, recovery_turn)
            .expect("explicit recovery handoff")
            .is_some()
    );
    assert!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("resolved coalesced OwnerResume")
            .is_none()
    );
}

#[tokio::test]
async fn synthetic_explicit_stop_discards_pending_crash_owner_resume() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let crashed_turn = TurnId::new();
    let crashed_admission = repository
        .admit_owner_resume_turn(owner.id, crashed_turn, requests[0].request_id)
        .await
        .expect("initial OwnerResume admission")
        .expect("initial OwnerResume admitted");
    assert_eq!(
        repository
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                crashed_admission.admission_id,
                crashed_turn,
                128,
            )
            .expect("safe child-result delivery")
            .history_item_ids,
        vec![requests[0].request_id.0]
    );
    let target = repository
        .captured_running_terminal_target(owner.id)
        .await
        .expect("capture OwnerResume")
        .expect("running OwnerResume");
    assert!(
        repository
            .recover_captured_running_session_with_protocol_event(
                owner.id,
                &failed_terminal(owner.id, "owner process crashed"),
                target,
            )
            .await
            .expect("recover crashed OwnerResume")
    );
    let explicit = repository
        .append_inter_agent_communication_with_protocol_bundle(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "explicit recovery work",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("explicit recovery trigger");
    assert!(explicit.schedule_turn);
    assert!(matches!(
        repository
            .settle_pending_agent_trigger_with_terminal(
                owner.id,
                explicit.history_item_id,
                pre_admission_interrupted_terminal(),
            )
            .expect("synthetic stop explicit recovery"),
        PendingAgentTriggerSettlement::Applied { handoff: None, .. }
    ));
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("discarded crash effects")
            .deferred
            .expect("crash deferred receipt")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("resolved coalesced OwnerResume")
            .is_none()
    );
}

#[tokio::test]
async fn legacy_completed_early_blocks_synthetic_terminal_until_exact_stop() {
    for terminal in [
        pre_admission_failed_terminal("launch failed while owner awaits descendants"),
        pre_admission_agent_interrupted_terminal(),
    ] {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let owner = retained_test_agent(
            &store,
            root_session_id,
            root_session_id,
            "/root",
            "completed_owner",
        )
        .await;
        let child = retained_test_agent(
            &store,
            root_session_id,
            owner.id,
            "/root/completed_owner",
            "child",
        )
        .await;
        let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
        let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission,
                    &completed_terminal_for_response(owner.id, None),
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("early owner completion"),
            AdmittedTerminalCommit::Applied
        );
        replace_current_handoff_with_legacy_completed_early(
            &store,
            owner.id,
            owner_turn,
            root_session_id,
        );
        let explicit = repository
            .append_inter_agent_communication_with_protocol_bundle_and_capacity(
                owner.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/completed_owner".to_string(),
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        "/root/completed_owner",
                        "/root",
                        "preserve this exact trigger",
                    ),
                    trigger_turn: true,
                },
                false,
                false,
            )
            .expect("completed-owner explicit trigger");
        assert!(!explicit.schedule_turn);

        for _ in 0..2 {
            assert_eq!(
                repository
                    .settle_pending_agent_trigger_with_terminal(
                        owner.id,
                        explicit.history_item_id,
                        terminal.clone(),
                    )
                    .expect("blocked non-destructive settlement"),
                PendingAgentTriggerSettlement::BlockedByPendingDeferredCompletion {
                    deferred_turn_id: owner_turn
                }
            );
            assert_eq!(
                repository
                    .pending_agent_trigger_history_item_id(owner.id)
                    .expect("retained exact trigger"),
                Some(explicit.history_item_id)
            );
            let deferred = repository
                .pending_deferred_completion(owner.id)
                .expect("pending deferred query")
                .expect("completed-early deferred remains pending");
            assert_eq!(deferred.agent_turn_id, owner_turn);
            assert_eq!(deferred.state, DeferredAgentCompletionState::Pending);
        }

        assert!(matches!(
            repository
                .settle_pending_agent_trigger_with_terminal(
                    owner.id,
                    explicit.history_item_id,
                    pre_admission_user_stopped_terminal(),
                )
                .expect("destructive stop settlement"),
            PendingAgentTriggerSettlement::Applied { handoff: None, .. }
        ));
        assert!(
            repository
                .pending_agent_trigger_history_item_id(owner.id)
                .expect("settled trigger")
                .is_none()
        );
        assert_eq!(
            repository
                .agent_terminal_effects(owner.id, owner_turn)
                .expect("discarded deferred effects")
                .deferred
                .expect("completed-early receipt")
                .state,
            DeferredAgentCompletionState::Discarded
        );
    }
}

#[tokio::test]
async fn legacy_completed_early_and_current_crash_deferred_remain_compatible() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let owner = retained_test_agent(
        &store,
        root_session_id,
        root_session_id,
        "/root",
        "completed_owner",
    )
    .await;
    let child = retained_test_agent(
        &store,
        root_session_id,
        owner.id,
        "/root/completed_owner",
        "child",
    )
    .await;
    let (owner_admission, owner_turn) = active_turn(&store, owner.id).await;
    let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission,
                &completed_terminal_for_response(owner.id, None),
                owner_turn,
                None,
                None,
            )
            .await
            .expect("early owner completion"),
        AdmittedTerminalCommit::Applied
    );
    replace_current_handoff_with_legacy_completed_early(
        &store,
        owner.id,
        owner_turn,
        root_session_id,
    );
    let completed_explicit = repository
        .append_inter_agent_communication_with_protocol_bundle_and_capacity(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/completed_owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/completed_owner",
                    "/root",
                    "must remain deferred",
                ),
                trigger_turn: true,
            },
            false,
            false,
        )
        .expect("completed-owner explicit trigger");
    assert!(
        !completed_explicit.schedule_turn,
        "completed-early owner mail must remain queued until descendant settlement"
    );
    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen completed-early queue");
    reopened_sqlite.migrate().expect("migrate reopened store");
    let reopened = StoreBundle::new(reopened_sqlite);
    let projection = reopened
        .protocol_event_store()
        .retained_descendant_page(root_session_id, 0, 10)
        .expect("reopened completed-early projection");
    let projected_owner = projection
        .items
        .iter()
        .find(|item| item.edge.child_session_id == owner.id)
        .expect("projected completed-early owner");
    assert_eq!(
        projected_owner.pending_trigger_history_item_id,
        Some(completed_explicit.history_item_id)
    );
    assert!(!projected_owner.pending_trigger_schedule_ready);
    assert_eq!(projected_owner.pending_owner_resume_request_id, None);
    assert!(
            repository
                .admit_agent_triggered_turn(
                    owner.id,
                    TurnId::new(),
                    completed_explicit.history_item_id,
                )
                .await
                .expect("completed-owner explicit admission")
                .is_none()
        );
    assert!(matches!(
        repository
            .settle_pending_agent_trigger_with_terminal(
                owner.id,
                completed_explicit.history_item_id,
                pre_admission_user_stopped_terminal(),
            )
            .expect("completed-owner synthetic settlement"),
        PendingAgentTriggerSettlement::Applied { handoff: None, .. }
    ));
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, owner_turn)
            .expect("stopped completed-early effects")
            .deferred
            .expect("completed-early deferred")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        repository
            .pending_agent_trigger_history_item_id(owner.id)
            .expect("settled completed-owner trigger")
            .is_none()
    );

    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let owner = retained_test_agent(
        &store,
        root_session_id,
        root_session_id,
        "/root",
        "crashed_owner",
    )
    .await;
    let child = retained_test_agent(
        &store,
        root_session_id,
        owner.id,
        "/root/crashed_owner",
        "child",
    )
    .await;
    let (_owner_admission, crashed_turn) = active_turn(&store, owner.id).await;
    let (_child_admission, _child_turn) = active_turn(&store, child.id).await;
    let target = repository
        .captured_running_terminal_target(owner.id)
        .await
        .expect("capture crashed owner")
        .expect("running crashed owner");
    assert!(
        repository
            .recover_captured_running_session_with_protocol_event(
                owner.id,
                &failed_terminal(owner.id, "owner process crashed"),
                target,
            )
            .await
            .expect("recover owner without OwnerResume")
    );
    assert!(
        repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("owner resume requests")
            .is_empty()
    );
    let capacity_error = repository
        .append_inter_agent_communication_with_protocol_bundle_and_capacity(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/crashed_owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/crashed_owner",
                    "/root",
                    "capacity-rejected crash recovery",
                ),
                trigger_turn: true,
            },
            false,
            false,
        )
        .expect_err("ready orphan crash must require an execution reservation");
    assert!(matches!(
        capacity_error,
        StorageError::AgentCapacityUnavailable { session_id }
            if session_id == owner.id
    ));
    assert!(
        repository
            .pending_agent_trigger_history_item_id(owner.id)
            .expect("capacity-rejected crash trigger")
            .is_none()
    );
    let crash_explicit = repository
        .append_inter_agent_communication_with_protocol_bundle(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/crashed_owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/crashed_owner",
                    "/root",
                    "recover orphan crash",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("orphan crash explicit trigger");
    assert!(crash_explicit.schedule_turn);
    let projection = store
        .protocol_event_store()
        .retained_descendant_page(root_session_id, 0, 10)
        .expect("orphan crash projection");
    let projected_owner = projection
        .items
        .iter()
        .find(|item| item.edge.child_session_id == owner.id)
        .expect("projected crashed owner");
    assert_eq!(
        projected_owner.pending_trigger_history_item_id,
        Some(crash_explicit.history_item_id)
    );
    assert!(projected_owner.pending_trigger_schedule_ready);
    assert_eq!(projected_owner.pending_owner_resume_request_id, None);

    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen orphan crash store");
    reopened_sqlite.migrate().expect("migrate reopened store");
    let reopened = StoreBundle::new(reopened_sqlite);
    let reopened_projection = reopened
        .protocol_event_store()
        .retained_descendant_page(root_session_id, 0, 10)
        .expect("reopened orphan crash projection");
    let reopened_owner = reopened_projection
        .items
        .iter()
        .find(|item| item.edge.child_session_id == owner.id)
        .expect("reopened crashed owner");
    assert_eq!(
        reopened_owner.pending_trigger_history_item_id,
        Some(crash_explicit.history_item_id)
    );
    assert!(reopened_owner.pending_trigger_schedule_ready);
    assert_eq!(reopened_owner.pending_owner_resume_request_id, None);

    let recovery_turn = TurnId::new();
    let recovery = reopened
        .session_repo()
        .admit_agent_triggered_turn(owner.id, recovery_turn, crash_explicit.history_item_id)
        .await
        .expect("reopened explicit crash recovery")
        .expect("explicit crash recovery admitted");
    assert_eq!(
        reopened
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                recovery.admission_id,
                recovery_turn,
                128,
            )
            .expect("safe orphan-crash trigger delivery")
            .history_item_ids,
        vec![crash_explicit.history_item_id]
    );
    assert_eq!(
        reopened
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                recovery.admission_id,
                &completed_terminal_for_response(owner.id, None),
                recovery_turn,
                None,
                None,
            )
            .await
            .expect("explicit orphan-crash recovery terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        reopened
            .session_repo()
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("superseded orphan crash")
            .deferred
            .expect("orphan crash deferred")
            .state,
        DeferredAgentCompletionState::Superseded
    );
    assert!(
        reopened
            .session_repo()
            .agent_terminal_effects(owner.id, recovery_turn)
            .expect("replacement completion effects")
            .deferred
            .is_none(),
        "normal recovery completion no longer creates completed_early"
    );
    assert!(
        reopened
            .session_repo()
            .agent_completion_handoff(owner.id, recovery_turn)
            .expect("replacement completion handoff")
            .is_some(),
        "normal recovery completion publishes directly to its immediate parent"
    );
}

#[tokio::test]
async fn orphan_crash_explicit_failure_and_interruption_resolve_deferred_receipt() {
    let (store, owner, crashed_turn, explicit) =
        orphan_crash_explicit_fixture("failed_recovery").await;
    let repository = store.session_repo();
    let failed_turn = TurnId::new();
    let failed_admission = repository
        .admit_agent_triggered_turn(owner.id, failed_turn, explicit.history_item_id)
        .await
        .expect("failed explicit recovery admission")
        .expect("failed explicit recovery admitted");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                failed_admission.admission_id,
                &failed_terminal(owner.id, "explicit recovery failed"),
                failed_turn,
                None,
                None,
            )
            .await
            .expect("failed explicit recovery terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("superseded orphan crash")
            .deferred
            .expect("orphan crash deferred")
            .state,
        DeferredAgentCompletionState::Superseded
    );
    assert!(
        repository
            .agent_completion_handoff(owner.id, failed_turn)
            .expect("failed explicit recovery handoff")
            .is_some()
    );
    assert!(
        repository
            .pending_deferred_completion(owner.id)
            .expect("failed explicit recovery deferred")
            .is_none()
    );

    let (store, owner, crashed_turn, explicit) =
        orphan_crash_explicit_fixture("stopped_recovery").await;
    let repository = store.session_repo();
    let stopped_turn = TurnId::new();
    let stopped_admission = repository
        .admit_agent_triggered_turn(owner.id, stopped_turn, explicit.history_item_id)
        .await
        .expect("stopped explicit recovery admission")
        .expect("stopped explicit recovery admitted");
    let stopped = RunEvent::TurnTerminal {
        session_id: owner.id,
        terminal: Box::new(pre_admission_agent_interrupted_terminal()),
    };
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                stopped_admission.admission_id,
                &stopped,
                stopped_turn,
                None,
                None,
            )
            .await
            .expect("stopped explicit recovery terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .agent_terminal_effects(owner.id, crashed_turn)
            .expect("discarded orphan crash")
            .deferred
            .expect("orphan crash deferred")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        repository
            .agent_completion_handoff(owner.id, stopped_turn)
            .expect("stopped explicit recovery handoff")
            .is_none()
    );
    assert!(
        repository
            .pending_deferred_completion(owner.id)
            .expect("stopped explicit recovery deferred")
            .is_none()
    );
}

#[tokio::test]
async fn pending_agent_trigger_exact_admission_remains_claimed_after_store_restart() {
    let (store, root_session_id) = test_repo().await;
    let (child, trigger_history_item_id, _) =
        spawn_pending_child(&store, root_session_id, "exact").await;
    let child_turn_id = TurnId::new();

    let admission = store
        .session_repo()
        .admit_agent_triggered_turn(child.id, child_turn_id, trigger_history_item_id)
        .await
        .expect("exact trigger admission")
        .expect("pending trigger admitted");
    assert_eq!(
        store
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending trigger query"),
        Some(trigger_history_item_id),
        "admission owns execution, but the mailbox row remains pending until the safe context boundary"
    );

    let paths = store.paths().clone();
    drop(store);
    let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
    reopened_sqlite.migrate().expect("migrate reopened store");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert_eq!(
        reopened
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("reopened pending trigger query"),
        Some(trigger_history_item_id)
    );
    let descendants = reopened
        .protocol_event_store()
        .retained_descendant_page(root_session_id, 0, 10)
        .expect("restarted descendant projection");
    assert_eq!(descendants.items.len(), 1);
    assert_eq!(
        descendants.items[0].pending_trigger_history_item_id,
        Some(trigger_history_item_id)
    );
    assert_eq!(
        reopened
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                child.id,
                admission.admission_id,
                child_turn_id,
                128,
            )
            .expect("restarted safe trigger delivery")
            .history_item_ids,
        vec![trigger_history_item_id]
    );
    assert_eq!(
        reopened
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("delivered trigger query"),
        None
    );
}

#[tokio::test]
async fn admitted_explicit_wake_abort_settles_only_its_claim_and_survives_reopen() {
    let (store, root_session_id) = test_repo().await;
    let (child, first_history_item_id, _) =
        spawn_pending_child(&store, root_session_id, "exact_abort").await;
    let repository = store.session_repo();
    let second = repository
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/exact_abort".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/exact_abort",
                    "/root",
                    "later independent wake",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("later explicit wake");
    let turn_id = TurnId::new();
    let admission = repository
        .admit_agent_triggered_turn(child.id, turn_id, first_history_item_id)
        .await
        .expect("explicit admission")
        .expect("explicit wake admitted");

    let terminal = pre_admission_agent_interrupted_terminal();
    assert!(matches!(
        repository
            .settle_agent_execution_wake_with_terminal(
                child.id,
                AgentExecutionWakeTerminalOwner::ExplicitTask(first_history_item_id),
                terminal.clone(),
            )
            .expect("exact wake settlement"),
        AgentExecutionWakeTerminalSettlement::Applied {
            turn_id: observed_turn_id,
            terminal: observed_terminal,
        } if observed_turn_id == turn_id
            && observed_terminal.outcome == terminal.outcome
    ));
    assert!(matches!(
        repository
            .settle_agent_execution_wake_with_terminal(
                child.id,
                AgentExecutionWakeTerminalOwner::ExplicitTask(first_history_item_id),
                terminal.clone(),
            )
            .expect("idempotent exact wake settlement"),
        AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
            turn_id: observed_turn_id,
            terminal: observed_terminal,
        } if observed_turn_id == turn_id
            && observed_terminal.outcome == terminal.outcome
    ));

    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT admission_id, turn_id
                         FROM agent_trigger_turn_claims
                         WHERE history_item_id = ?1",
                    [first_history_item_id.to_string()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .expect("durable wake claim"),
            (admission.admission_id.to_string(), turn_id.to_string())
        );
        let mut statement = connection
            .prepare(
                "SELECT id, state
                     FROM agent_mailbox_messages
                     WHERE id IN (?1, ?2)
                     ORDER BY CASE id WHEN ?1 THEN 0 ELSE 1 END",
            )
            .expect("mailbox states");
        let states = statement
            .query_map(
                params![
                    first_history_item_id.to_string(),
                    second.history_item_id.to_string()
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("mailbox state rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("mailbox states");
        assert_eq!(
            states,
            vec![
                (first_history_item_id.to_string(), "discarded".to_string()),
                (second.history_item_id.to_string(), "pending".to_string()),
            ]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("tree fence count"),
            0,
            "an exact task abort must not create a subtree fence"
        );
    }

    let paths = store.paths().clone();
    drop(store);
    let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
    reopened_sqlite
        .migrate()
        .expect("validate reopened V53 storage");
    let reopened = StoreBundle::new(reopened_sqlite);
    let reopened_terminal = reopened
        .session_repo()
        .durable_terminal_for_turn(child.id, turn_id)
        .await
        .expect("reopened terminal")
        .expect("reopened exact terminal");
    assert_eq!(reopened_terminal.outcome, terminal.outcome);
    assert_eq!(
        reopened
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("later wake remains pending after reopen"),
        Some(second.history_item_id)
    );
}

#[tokio::test]
async fn explicit_wake_abort_reports_an_existing_terminal_instead_of_rewriting_it() {
    let (store, root_session_id) = test_repo().await;
    let (child, history_item_id, _) =
        spawn_pending_child(&store, root_session_id, "terminal_wins").await;
    let repository = store.session_repo();
    let turn_id = TurnId::new();
    let admission = repository
        .admit_agent_triggered_turn(child.id, turn_id, history_item_id)
        .await
        .expect("explicit admission")
        .expect("explicit wake admitted");
    let failed = pre_admission_failed_terminal("durable failure won first");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                admission.admission_id,
                &RunEvent::TurnTerminal {
                    session_id: child.id,
                    terminal: Box::new(failed.clone()),
                },
                turn_id,
                None,
                None,
            )
            .await
            .expect("durable failure"),
        AdmittedTerminalCommit::Applied
    );

    assert!(matches!(
        repository
            .settle_agent_execution_wake_with_terminal(
                child.id,
                AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id),
                pre_admission_agent_interrupted_terminal(),
            )
            .expect("late hard-abort settlement"),
        AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
            turn_id: observed_turn_id,
            terminal: observed_terminal,
        } if observed_turn_id == turn_id
            && observed_terminal.outcome == failed.outcome
    ));
}

#[tokio::test]
async fn admitted_explicit_wake_terminal_resolves_only_its_claim_before_safe_delivery() {
    let cases = [
        (
            "completed",
            DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
            "delivered",
        ),
        (
            "failed",
            pre_admission_failed_terminal("setup failed before mailbox delivery"),
            "delivered",
        ),
        (
            "interrupted",
            pre_admission_agent_interrupted_terminal(),
            "discarded",
        ),
    ];

    for (case, terminal, expected_first_state) in cases {
        let (store, root_session_id) = test_repo().await;
        let child_name = format!("resolve_{case}");
        let child_path = format!("/root/{child_name}");
        let (child, first_history_item_id, _) =
            spawn_pending_child(&store, root_session_id, &child_name).await;
        let repository = store.session_repo();
        let second = repository
            .append_inter_agent_communication_with_protocol_bundle(
                child.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: child_path,
                    content: render_inter_agent_message(
                        InterAgentMessageType::NewTask,
                        &format!("/root/{child_name}"),
                        "/root",
                        "later independent wake",
                    ),
                    trigger_turn: true,
                },
                false,
            )
            .expect("later explicit wake");
        let turn_id = TurnId::new();
        let admission = repository
            .admit_agent_triggered_turn(child.id, turn_id, first_history_item_id)
            .await
            .expect("explicit admission")
            .expect("explicit wake admitted");
        let event = RunEvent::TurnTerminal {
            session_id: child.id,
            terminal: Box::new(terminal.clone()),
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                    child.id,
                    admission.admission_id,
                    &event,
                    turn_id,
                    None,
                    false,
                    None,
                )
                .await
                .expect("terminal before safe mailbox delivery"),
            AdmittedTerminalCommit::Applied,
            "{case}"
        );

        {
            let connection = repository.connection.lock().expect("sqlite mutex");
            let first = connection
                .query_row(
                    "SELECT state, delivered_turn_id,
                                resolved_by_terminal_event_id
                         FROM agent_mailbox_messages
                         WHERE id = ?1",
                    [first_history_item_id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .expect("claimed wake state");
            assert_eq!(first.0, expected_first_state, "{case}");
            if expected_first_state == "delivered" {
                assert_eq!(first.1, Some(turn_id.to_string()), "{case}");
                assert_eq!(first.2, None, "{case}");
            } else {
                assert_eq!(first.1, None, "{case}");
                assert!(first.2.is_some(), "{case}");
            }
            assert_eq!(
                connection
                    .query_row(
                        "SELECT state
                             FROM agent_mailbox_messages
                             WHERE id = ?1",
                        [second.history_item_id.to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .expect("later wake state"),
                "pending",
                "{case}"
            );
        }

        assert!(matches!(
            repository
                .settle_agent_execution_wake_with_terminal(
                    child.id,
                    AgentExecutionWakeTerminalOwner::ExplicitTask(first_history_item_id),
                    pre_admission_agent_interrupted_terminal(),
                )
                .expect("idempotent wake settlement"),
            AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
                turn_id: observed_turn_id,
                terminal: observed_terminal,
            } if observed_turn_id == turn_id
                && observed_terminal.outcome == terminal.outcome
        ));

        let paths = store.paths().clone();
        drop(repository);
        drop(store);
        let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
        reopened_sqlite
            .migrate()
            .expect("reopen validates exact V53 claim resolution");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(child.id)
                .expect("later wake remains schedulable"),
            Some(second.history_item_id),
            "{case}"
        );
        assert!(
            reopened
                .session_repo()
                .admit_agent_triggered_turn(child.id, TurnId::new(), second.history_item_id,)
                .await
                .expect("later wake admission")
                .is_some(),
            "{case}: later wake must not collide with the resolved claim"
        );
    }
}

#[tokio::test]
async fn explicit_wake_admission_and_abort_are_atomic_across_connections() {
    let (store, root_session_id) = test_repo().await;
    let (child, history_item_id, _) =
        spawn_pending_child(&store, root_session_id, "claim_abort_race").await;
    let later = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/claim_abort_race".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/claim_abort_race",
                    "/root",
                    "later wake must survive either race winner",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("later explicit wake");
    let competing_sqlite = SqliteStore::open(store.paths()).expect("second sqlite connection");
    competing_sqlite
        .migrate()
        .expect("migrate second connection");
    let competing = StoreBundle::new(competing_sqlite);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let admitted_turn_id = TurnId::new();

    let (admission, settlement) = std::thread::scope(|scope| {
        let admission_barrier = barrier.clone();
        let admission_store = &store;
        let admission_thread = scope.spawn(move || {
            admission_barrier.wait();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("admission runtime")
                .block_on(admission_store.session_repo().admit_agent_triggered_turn(
                    child.id,
                    admitted_turn_id,
                    history_item_id,
                ))
        });
        let settlement_barrier = barrier.clone();
        let settlement_store = &competing;
        let settlement_thread = scope.spawn(move || {
            settlement_barrier.wait();
            settlement_store
                .session_repo()
                .settle_agent_execution_wake_with_terminal(
                    child.id,
                    AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id),
                    pre_admission_agent_interrupted_terminal(),
                )
        });
        (
            admission_thread.join().expect("admission thread"),
            settlement_thread.join().expect("settlement thread"),
        )
    });
    let admission = admission.expect("admission result");
    let settlement = settlement.expect("settlement result");
    let settled_turn_id = match settlement {
        AgentExecutionWakeTerminalSettlement::Applied { turn_id, .. } => turn_id,
        other => panic!("abort must own the selected wake exactly once: {other:?}"),
    };
    match admission {
        Some(admission) => {
            assert_eq!(settled_turn_id, admitted_turn_id);
            let repository = store.session_repo();
            let connection = repository.connection.lock().expect("sqlite mutex");
            assert_eq!(
                connection
                    .query_row(
                        "SELECT admission_id
                             FROM agent_trigger_turn_claims
                             WHERE history_item_id = ?1",
                        [history_item_id.to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .expect("winning admission claim"),
                admission.admission_id.to_string()
            );
        }
        None => {
            assert_ne!(
                settled_turn_id, admitted_turn_id,
                "pre-admission abort owns a synthetic exact turn"
            );
        }
    }
    let terminal_events = store
        .protocol_event_store()
        .list_runtime_events_for_session(child.id)
        .expect("terminal events")
        .into_iter()
        .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
        .count();
    assert_eq!(terminal_events, 1);
    assert_eq!(
        store
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("later wake after race"),
        Some(later.history_item_id),
        "neither race winner may retire a later explicit wake"
    );

    let paths = store.paths().clone();
    drop(competing);
    drop(store);
    let reopened_sqlite = SqliteStore::open(&paths).expect("reopen race store");
    reopened_sqlite
        .migrate()
        .expect("reopen validates race winner claim");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert_eq!(
        reopened
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("reopened later wake"),
        Some(later.history_item_id)
    );
    assert!(
        reopened
            .session_repo()
            .admit_agent_triggered_turn(child.id, TurnId::new(), later.history_item_id)
            .await
            .expect("reopened later wake admission")
            .is_some(),
        "the later wake must remain independently admissible after reopen"
    );
}

#[tokio::test]
async fn pending_agent_trigger_old_lease_coalesces_newer_session_mail_into_one_turn() {
    let (store, root_session_id) = test_repo().await;
    let (child, first_trigger_history_item_id, _) =
        spawn_pending_child(&store, root_session_id, "coalesced").await;
    let second_trigger = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/coalesced".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/coalesced",
                    "/root",
                    "include this follow-up in the same pending batch",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("newer pending trigger");
    assert!(second_trigger.schedule_turn);
    assert_eq!(
        store
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("oldest schedulable pending trigger"),
        Some(first_trigger_history_item_id)
    );

    // One admitted turn consumes all session-scoped mail that precedes its
    // SessionStarted append. Keeping the older exact lease valid here avoids
    // losing or repeatedly rescheduling the already-reserved execution when
    // another message arrives before admission.
    let child_turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_agent_triggered_turn(child.id, child_turn_id, first_trigger_history_item_id)
        .await
        .expect("coalesced exact trigger admission")
        .expect("older reserved trigger remains admissible");
    assert_eq!(
        store
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending trigger before safe delivery"),
        Some(first_trigger_history_item_id)
    );
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                child.id,
                admission.admission_id,
                child_turn_id,
                128,
            )
            .expect("coalesced safe mailbox delivery")
            .history_item_ids,
        vec![
            first_trigger_history_item_id,
            second_trigger.history_item_id
        ]
    );
    assert_eq!(
        store
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending trigger after safe delivery"),
        None
    );
    assert!(
        store
            .session_repo()
            .admit_agent_triggered_turn(child.id, TurnId::new(), second_trigger.history_item_id)
            .await
            .expect("newer trigger retry")
            .is_none(),
        "the same SessionStarted append must claim both pending triggers"
    );
    let events = store
        .protocol_event_store()
        .list_runtime_events(child.id, child_turn_id)
        .expect("coalesced turn events");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.msg, RuntimeEventMsg::Warning { .. }))
            .count(),
        1
    );
}

#[test]
fn pending_agent_terminal_rejects_cumulative_metrics_before_admission() {
    let mut terminal = pre_admission_failed_terminal("launch did not start");
    validate_pending_agent_terminal(&terminal).expect("unstarted terminal without metrics");
    terminal.metrics.cumulative_token_usage = Some(crate::session::CumulativeTokenUsage::default());
    assert!(validate_pending_agent_terminal(&terminal).is_err());
}

#[tokio::test]
async fn pending_agent_trigger_synthetic_failed_commits_terminal_final_and_receipt_once() {
    let (store, root_session_id) = test_repo().await;
    let (child, trigger_history_item_id, root_turn_id) =
        spawn_pending_child(&store, root_session_id, "failed").await;
    let terminal = pre_admission_failed_terminal("launch failed before admission");

    let (child_turn_id, handoff) = match store
        .session_repo()
        .settle_pending_agent_trigger_with_terminal(
            child.id,
            trigger_history_item_id,
            terminal.clone(),
        )
        .expect("synthetic failure settlement")
    {
        PendingAgentTriggerSettlement::Applied { turn_id, handoff } => {
            (turn_id, handoff.expect("failed child handoff"))
        }
        other => panic!("pending trigger was unexpectedly unavailable: {other:?}"),
    };
    assert_eq!(stored_admission_state(&store, child.id).0, "failed");
    let child_events = store
        .protocol_event_store()
        .list_runtime_events(child.id, child_turn_id)
        .expect("synthetic child events");
    assert_eq!(
        child_events
            .iter()
            .filter(|event| matches!(
                &event.msg,
                RuntimeEventMsg::Warning { message }
                    if message.starts_with("thread started:")
            ))
            .count(),
        1
    );
    assert_eq!(
        child_events
            .iter()
            .filter(|event| matches!(
                &event.msg,
                RuntimeEventMsg::TurnTerminal { terminal: stored }
                    if matches!(
                        &stored.outcome,
                        TurnTerminalOutcome::Failed { error }
                            if error == "launch failed before admission"
                    )
                        && stored.final_response_id.is_none()
                        && stored.tool_call_count == 0
                        && stored.failed_tool_count == 0
                        && stored.change_count == 0
            ))
            .count(),
        1
    );
    assert_eq!(handoff.child_session_id, child.id);
    assert_eq!(handoff.child_turn_id, child_turn_id);
    assert_eq!(handoff.parent_session_id, root_session_id);
    assert_eq!(
        store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("completion receipt query"),
        Some(handoff.clone())
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(root_session_id, root_turn_id)
            .expect("parent history before safe delivery")
            .into_iter()
            .all(|item| item.id != handoff.history_item_id)
    );
    let parent_finals = store
        .session_repo()
        .agent_mailbox_communications_by_id(root_session_id, &[handoff.history_item_id])
        .expect("queued parent FINAL");
    assert!(matches!(
        parent_finals.as_slice(),
        [(id, communication)]
            if *id == handoff.history_item_id
                && communication.author == "/root/failed"
                && communication.recipient == "/root"
                && !communication.trigger_turn
    ));

    assert_eq!(
            store
                .session_repo()
                .settle_pending_agent_trigger_with_terminal(
                    child.id,
                    trigger_history_item_id,
                    terminal,
                )
                .expect("idempotent synthetic failure retry"),
            PendingAgentTriggerSettlement::WakeOwnedOrResolved
        );
    assert_eq!(
        store
            .protocol_event_store()
            .list_runtime_events(child.id, child_turn_id)
            .expect("events after retry")
            .len(),
        child_events.len()
    );
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM agent_completion_handoffs
                     WHERE child_session_id = ?1 AND child_turn_id = ?2",
                params![child.id.to_string(), child_turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("completion receipt count"),
        1
    );
}

#[tokio::test]
async fn synthetic_failed_terminal_delivers_only_its_exact_pending_trigger() {
    let (store, root_session_id) = test_repo().await;
    let (child, first_trigger_history_item_id, _root_turn_id) =
        spawn_pending_child(&store, root_session_id, "exact_synthetic").await;
    let repository = store.session_repo();
    let second = repository
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/exact_synthetic".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/exact_synthetic",
                    "/root",
                    "later independent trigger",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("second pending trigger");

    let synthetic_turn_id = match repository
        .settle_pending_agent_trigger_with_terminal(
            child.id,
            first_trigger_history_item_id,
            pre_admission_failed_terminal("first launch failed"),
        )
        .expect("exact synthetic terminal")
    {
        PendingAgentTriggerSettlement::Applied {
            turn_id,
            handoff: Some(_),
        } => turn_id,
        other => panic!("synthetic failure was unexpectedly unavailable: {other:?}"),
    };

    let mailbox_states = {
        let connection = repository.connection.lock().expect("sqlite mutex");
        let mut statement = connection
            .prepare(
                "SELECT id, state, delivered_turn_id
                 FROM agent_mailbox_messages
                 WHERE recipient_session_id = ?1
                   AND id IN (?2, ?3)
                 ORDER BY CASE id WHEN ?2 THEN 0 ELSE 1 END",
            )
            .expect("mailbox state query");
        statement
            .query_map(
                params![
                    child.id.to_string(),
                    first_trigger_history_item_id.to_string(),
                    second.history_item_id.to_string(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .expect("mailbox state rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("mailbox states after exact settlement")
    };
    assert_eq!(
        mailbox_states,
        vec![
            (
                first_trigger_history_item_id.to_string(),
                "delivered".to_string(),
                Some(synthetic_turn_id.to_string()),
            ),
            (
                second.history_item_id.to_string(),
                "pending".to_string(),
                None,
            )
        ]
    );
    assert_eq!(
        repository
            .pending_agent_trigger_history_item_id(child.id)
            .expect("later trigger remains schedulable"),
        Some(second.history_item_id)
    );
    let retained = store
        .protocol_event_store()
        .retained_descendant_snapshot(root_session_id, 1)
        .expect("fresh retained projection after exact settlement");
    assert_eq!(
        retained[0].pending_trigger_history_item_id,
        Some(second.history_item_id)
    );
    assert!(retained[0].pending_trigger_schedule_ready);
    assert!(
        repository
            .admit_agent_triggered_turn(child.id, TurnId::new(), second.history_item_id)
            .await
            .expect("later exact trigger admission")
            .is_some(),
        "the untouched later trigger must remain an admissible scheduler owner"
    );
}

#[tokio::test]
async fn pending_agent_trigger_synthetic_interrupted_does_not_create_handoff() {
    let (store, root_session_id) = test_repo().await;
    let (child, trigger_history_item_id, root_turn_id) =
        spawn_pending_child(&store, root_session_id, "interrupted").await;
    let terminal = pre_admission_interrupted_terminal();

    let child_turn_id = match store
        .session_repo()
        .settle_pending_agent_trigger_with_terminal(
            child.id,
            trigger_history_item_id,
            terminal.clone(),
        )
        .expect("synthetic interrupted settlement")
    {
        PendingAgentTriggerSettlement::Applied { turn_id, handoff } => {
            assert_eq!(handoff, None);
            turn_id
        }
        other => panic!("pending trigger was unexpectedly unavailable: {other:?}"),
    };
    assert_eq!(stored_admission_state(&store, child.id).0, "cancelled");
    assert!(matches!(
        store
            .protocol_event_store()
            .list_runtime_events(child.id, child_turn_id)
            .expect("interrupted child events")
            .as_slice(),
        [RuntimeEvent {
            msg: RuntimeEventMsg::Warning { .. },
            ..
        }, RuntimeEvent {
            msg: RuntimeEventMsg::TurnTerminal { terminal: stored },
            ..
        }] if matches!(
            stored.outcome,
            TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::TreeStopped
            }
        )
            && stored.final_response_id.is_none()
            && stored.tool_call_count == 0
            && stored.failed_tool_count == 0
            && stored.change_count == 0
    ));
    assert!(
        store
            .session_repo()
            .agent_completion_handoff(child.id, child_turn_id)
            .expect("interrupted receipt query")
            .is_none()
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(root_session_id, root_turn_id)
            .expect("parent history")
            .into_iter()
            .all(|item| !matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
    );
}

#[tokio::test]
async fn pending_agent_trigger_receipt_insert_failure_rolls_back_and_remains_pending() {
    let (store, root_session_id) = test_repo().await;
    let (child, trigger_history_item_id, root_turn_id) =
        spawn_pending_child(&store, root_session_id, "rollback").await;
    let repository = store.session_repo();
    repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch(
            "CREATE TEMP TRIGGER abort_pending_agent_completion_receipt
                 BEFORE INSERT ON agent_completion_handoffs
                 BEGIN
                     SELECT RAISE(ABORT, 'injected pending completion receipt failure');
                 END;",
        )
        .expect("install receipt failure");

    let error = repository
        .settle_pending_agent_trigger_with_terminal(
            child.id,
            trigger_history_item_id,
            pre_admission_failed_terminal("must roll back"),
        )
        .expect_err("receipt failure must roll back the whole settlement");
    assert!(
        error
            .to_string()
            .contains("injected pending completion receipt failure")
    );
    assert_eq!(
        stored_admission_state(&store, child.id),
        ("idle".to_string(), None, None, None)
    );
    assert!(
        store
            .protocol_event_store()
            .list_runtime_events_for_session(child.id)
            .expect("child events after rollback")
            .is_empty()
    );
    assert_eq!(
        repository
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending trigger after rollback"),
        Some(trigger_history_item_id)
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(root_session_id, root_turn_id)
            .expect("parent history after rollback")
            .into_iter()
            .all(|item| !matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
    );
    assert_eq!(
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*) FROM agent_completion_handoffs",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("receipt count after rollback"),
        0
    );

    repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch("DROP TRIGGER abort_pending_agent_completion_receipt;")
        .expect("drop receipt failure");
    assert!(matches!(
        repository
            .settle_pending_agent_trigger_with_terminal(
                child.id,
                trigger_history_item_id,
                pre_admission_failed_terminal("retry succeeds"),
            )
            .expect("retry settlement"),
        PendingAgentTriggerSettlement::Applied {
            handoff: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn pending_agent_trigger_admission_and_synthetic_failure_have_one_durable_winner() {
    let (store, root_session_id) = test_repo().await;
    let (child, trigger_history_item_id, _root_turn_id) =
        spawn_pending_child(&store, root_session_id, "raced").await;
    let reopened_sqlite = SqliteStore::open(store.paths()).expect("second sqlite connection");
    reopened_sqlite
        .migrate()
        .expect("migrate second connection");
    let competing_store = StoreBundle::new(reopened_sqlite);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let admitted_turn_id = TurnId::new();

    let (admission, settlement) = std::thread::scope(|scope| {
        let admission_store = &store;
        let settlement_store = &competing_store;
        let child_session_id = child.id;
        let admission_barrier = barrier.clone();
        let admission_thread = scope.spawn(move || {
            admission_barrier.wait();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("admission runtime")
                .block_on(admission_store.session_repo().admit_agent_triggered_turn(
                    child_session_id,
                    admitted_turn_id,
                    trigger_history_item_id,
                ))
        });
        let settlement_barrier = barrier.clone();
        let settlement_thread = scope.spawn(move || {
            settlement_barrier.wait();
            settlement_store
                .session_repo()
                .settle_pending_agent_trigger_with_terminal(
                    child_session_id,
                    trigger_history_item_id,
                    pre_admission_failed_terminal("pre-admission owner lost the race"),
                )
        });
        (
            admission_thread.join().expect("admission thread"),
            settlement_thread.join().expect("settlement thread"),
        )
    });
    let admission = admission.expect("admission result");
    let settlement = settlement.expect("settlement result");

    match (admission, settlement) {
        (Some(snapshot), PendingAgentTriggerSettlement::WakeOwnedOrResolved) => {
            let state = stored_admission_state(&store, child.id);
            assert_eq!(state.0, "running");
            assert_eq!(state.2, Some(admitted_turn_id.to_string()));
            {
                let repository = store.session_repo();
                let connection = repository.connection.lock().expect("sqlite mutex");
                assert_eq!(
                    connection
                        .query_row(
                            "SELECT admission_id, turn_id
                                 FROM agent_trigger_turn_claims
                                 WHERE recipient_session_id = ?1
                                   AND history_item_id = ?2",
                            params![child.id.to_string(), trigger_history_item_id.to_string()],
                            |row| { Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)) },
                        )
                        .expect("winning admission claim"),
                    (
                        snapshot.admission_id.to_string(),
                        admitted_turn_id.to_string()
                    )
                );
            }
            assert!(
                store
                    .session_repo()
                    .agent_completion_handoff(child.id, admitted_turn_id)
                    .expect("winning admission handoff query")
                    .is_none()
            );
            assert_eq!(
                store
                    .session_repo()
                    .pending_agent_trigger_history_item_id(child.id)
                    .expect("claimed trigger after admission wins"),
                Some(trigger_history_item_id),
                "admission claims the wake before the later safe-delivery boundary"
            );
        }
        (
            None,
            PendingAgentTriggerSettlement::Applied {
                turn_id: synthetic_turn_id,
                handoff: Some(handoff),
            },
        ) => {
            assert_eq!(stored_admission_state(&store, child.id).0, "failed");
            assert_eq!(handoff.child_turn_id, synthetic_turn_id);
            assert_eq!(handoff.parent_session_id, root_session_id);
            assert_eq!(
                store
                    .session_repo()
                    .pending_agent_trigger_history_item_id(child.id)
                    .expect("pending trigger after synthetic settlement wins"),
                None
            );
        }
        other => {
            panic!("trigger race produced more or fewer than one durable winner: {other:?}")
        }
    }
    assert_eq!(
        store
            .protocol_event_store()
            .list_runtime_events_for_session(child.id)
            .expect("runtime events after race")
            .into_iter()
            .filter(|event| {
                matches!(
                    &event.msg,
                    RuntimeEventMsg::Warning { message }
                        if message.starts_with("thread started:")
                )
            })
            .count(),
        1,
        "admission and settlement must share one append-order claim boundary"
    );
}

#[tokio::test]
async fn owner_resume_admission_coalesces_all_pending_rows_and_terminal_resolves_them() {
    let (store, root_session_id) = test_repo().await;
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 2).await;
    let owner_turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_owner_resume_turn(owner.id, owner_turn_id, requests[0].request_id)
        .await
        .expect("owner-resume admission")
        .expect("owner resume admitted");
    assert!(
        store
            .session_repo()
            .list_pending_owner_resume_requests(owner.id)
            .expect("pending requests after claim")
            .is_empty()
    );
    {
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*)
                         FROM agent_owner_resume_requests
                         WHERE owner_session_id = ?1
                           AND state = 'claimed'
                           AND claimed_turn_id = ?2",
                    params![owner.id.to_string(), owner_turn_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("claimed resume count"),
            2
        );
    }
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                admission.admission_id,
                owner_turn_id,
                128,
            )
            .expect("safe coalesced OwnerResume delivery")
            .history_item_ids,
        requests
            .iter()
            .map(|request| request.request_id.0)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                admission.admission_id,
                &completed_terminal_for_response(owner.id, None),
                owner_turn_id,
                None,
                None,
            )
            .await
            .expect("owner terminal"),
        AdmittedTerminalCommit::Applied
    );
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1
                       AND state = 'resolved'
                       AND claimed_turn_id = ?2
                       AND resolved_at_ms IS NOT NULL",
                params![owner.id.to_string(), owner_turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("resolved resume count"),
        2
    );
}

#[tokio::test]
async fn admitted_owner_resume_abort_uses_the_claimed_turn_and_is_idempotent() {
    let (store, root_session_id) = test_repo().await;
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 2).await;
    let repository = store.session_repo();
    let turn_id = TurnId::new();
    repository
        .admit_owner_resume_turn(owner.id, turn_id, requests[0].request_id)
        .await
        .expect("owner-resume admission")
        .expect("owner resume admitted");
    let terminal = pre_admission_agent_interrupted_terminal();

    assert!(matches!(
        repository
            .settle_agent_execution_wake_with_terminal(
                owner.id,
                AgentExecutionWakeTerminalOwner::OwnerResume(requests[0].request_id),
                terminal.clone(),
            )
            .expect("owner-resume hard abort"),
        AgentExecutionWakeTerminalSettlement::Applied {
            turn_id: observed_turn_id,
            terminal: observed_terminal,
        } if observed_turn_id == turn_id
            && observed_terminal.outcome == terminal.outcome
    ));
    assert!(matches!(
        repository
            .settle_agent_execution_wake_with_terminal(
                owner.id,
                AgentExecutionWakeTerminalOwner::OwnerResume(requests[0].request_id),
                terminal.clone(),
            )
            .expect("idempotent owner-resume hard abort"),
        AgentExecutionWakeTerminalSettlement::AlreadyTerminal {
            turn_id: observed_turn_id,
            terminal: observed_terminal,
        } if observed_turn_id == turn_id
            && observed_terminal.outcome == terminal.outcome
    ));
    let connection = repository.connection.lock().expect("sqlite mutex");
    let owner_resume_rows = connection
        .prepare(
            "SELECT state, claimed_turn_id
                 FROM agent_owner_resume_requests
                 WHERE owner_session_id = ?1
                 ORDER BY source_history_item_id",
        )
        .expect("prepare owner-resume state query")
        .query_map(params![owner.id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .expect("query owner-resume states")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect owner-resume states");
    assert_eq!(
        owner_resume_rows,
        vec![
            ("resolved".to_string(), Some(turn_id.to_string())),
            ("resolved".to_string(), Some(turn_id.to_string())),
        ]
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM agent_tree_stop_fences", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("tree fence count"),
        0
    );
}

#[tokio::test]
async fn owner_resume_state_trigger_prevents_claim_aba_and_resolved_reopen() {
    let (store, root_session_id) = test_repo().await;
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    {
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert!(
            connection
                .execute(
                    "INSERT INTO agent_owner_resume_requests
                         (owner_session_id, source_session_id, source_history_item_id,
                          state, claimed_turn_id, created_at_ms, updated_at_ms,
                          claimed_at_ms, resolved_at_ms)
                         VALUES (?1, ?2, ?3, 'pending', NULL, 1, 1, NULL, NULL)",
                    params![
                        root_session_id.to_string(),
                        owner.id.to_string(),
                        requests[0].request_id.to_string()
                    ],
                )
                .is_err(),
            "root cannot own an OwnerResume request"
        );
    }
    let turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_owner_resume_turn(owner.id, turn_id, requests[0].request_id)
        .await
        .expect("owner-resume admission")
        .expect("owner resume admitted");
    {
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert!(
            connection
                .execute(
                    "UPDATE agent_owner_resume_requests
                         SET state = 'resolved',
                             claimed_turn_id = ?3,
                             claimed_at_ms = claimed_at_ms + 1,
                             resolved_at_ms = updated_at_ms + 1,
                             updated_at_ms = updated_at_ms + 1
                         WHERE owner_session_id = ?1
                           AND source_history_item_id = ?2",
                    params![
                        owner.id.to_string(),
                        requests[0].request_id.to_string(),
                        TurnId::new().to_string()
                    ],
                )
                .is_err(),
            "claimed identity cannot change while resolving"
        );
    }
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                admission.admission_id,
                turn_id,
                128,
            )
            .expect("safe OwnerResume delivery")
            .history_item_ids,
        vec![requests[0].request_id.0]
    );
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                admission.admission_id,
                &completed_terminal_for_response(owner.id, None),
                turn_id,
                None,
                None,
            )
            .await
            .expect("owner terminal"),
        AdmittedTerminalCommit::Applied
    );
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert!(
        connection
            .execute(
                "UPDATE agent_owner_resume_requests
                     SET state = 'pending',
                         claimed_turn_id = NULL,
                         claimed_at_ms = NULL,
                         resolved_at_ms = NULL,
                         updated_at_ms = updated_at_ms + 1
                     WHERE owner_session_id = ?1
                       AND source_history_item_id = ?2",
                params![owner.id.to_string(), requests[0].request_id.to_string()],
            )
            .is_err(),
        "resolved owner-resume requests are terminal"
    );
}

#[tokio::test]
#[cfg(any())]
async fn fresh_nested_followup_schedules_inactive_immediate_owner_without_restart() {
    let (store, root_session_id) = test_repo().await;
    let owner = create_sibling_session(&store, root_session_id, "owner").await;
    let nested = create_sibling_session(&store, root_session_id, "nested").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            owner.id,
            "/root/owner",
            "owner",
        )
        .await
        .expect("owner edge");
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            nested.id,
            "/root/owner/nested",
            "nested",
        )
        .await
        .expect("nested edge");
    let _ = active_turn(&store, root_session_id).await;

    let stored = repository
        .append_inter_agent_communication_with_protocol_bundle(
            nested.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner/nested".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/nested",
                    "/root",
                    "fresh nested follow-up",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("nested follow-up");

    assert!(stored.schedule_turn);
    assert_eq!(
        stored.scheduled_owner_resumes,
        vec![ScheduledOwnerResume {
            owner_session_id: owner.id,
            request_id: OwnerResumeRequestId::from(stored.history_item_id),
        }]
    );
    assert_eq!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("live owner-resume projection"),
        Some(OwnerResumeRequestId::from(stored.history_item_id))
    );
    assert!(
        repository
            .list_pending_owner_resume_requests(root_session_id)
            .expect("root resume rows")
            .is_empty()
    );
}

#[tokio::test]
#[cfg(any())]
async fn fresh_owner_blocks_nested_resume_but_expired_owner_does_not() {
    let (store, root_session_id) = test_repo().await;
    let owner = create_sibling_session(&store, root_session_id, "owner").await;
    let nested = create_sibling_session(&store, root_session_id, "nested").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            owner.id,
            "/root/owner",
            "owner",
        )
        .await
        .expect("owner edge");
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            nested.id,
            "/root/owner/nested",
            "nested",
        )
        .await
        .expect("nested edge");
    let (owner_admission_id, owner_turn_id) = active_turn(&store, owner.id).await;
    let fresh = repository
        .append_inter_agent_communication_with_protocol_bundle(
            nested.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner/nested".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/nested",
                    "/root",
                    "fresh-owner follow-up",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("fresh-owner nested trigger");
    assert!(fresh.scheduled_owner_resumes.is_empty());
    assert!(
        repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("fresh owner requests")
            .is_empty()
    );

    repository
        .inject_raw_runtime_state_for_corruption_test(
            owner.id,
            "running",
            Some(&owner_admission_id.to_string()),
            Some(&owner_turn_id.to_string()),
            Some(1),
        )
        .expect("expire owner admission");
    let expired = repository
        .append_inter_agent_communication_with_protocol_bundle(
            nested.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner/nested".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/nested",
                    "/root",
                    "expired-owner follow-up",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("expired-owner nested trigger");
    assert_eq!(expired.scheduled_owner_resumes.len(), 1);
    assert_eq!(
        expired.scheduled_owner_resumes[0].owner_session_id,
        owner.id
    );
}

#[tokio::test]
#[cfg(any())]
async fn corrupt_ancestor_admission_rolls_back_nested_trigger_and_resume_schedule() {
    let (store, root_session_id) = test_repo().await;
    let owner = create_sibling_session(&store, root_session_id, "owner").await;
    let nested = create_sibling_session(&store, root_session_id, "nested").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            owner.id,
            "/root/owner",
            "owner",
        )
        .await
        .expect("owner edge");
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            nested.id,
            "/root/owner/nested",
            "nested",
        )
        .await
        .expect("nested edge");
    repository
        .inject_raw_runtime_state_for_corruption_test(
            owner.id,
            "running",
            Some(&AdmissionId::new().to_string()),
            None,
            Some(i64::MAX - 1),
        )
        .expect("partial owner admission");
    let history_before = store
        .protocol_event_store()
        .list_history_items_for_session(nested.id)
        .expect("history before corruption append")
        .len();

    let error = repository
        .append_inter_agent_communication_with_protocol_bundle(
            nested.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner/nested".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/nested",
                    "/root",
                    "must roll back",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect_err("partial ancestor admission must fail closed");
    assert!(error.to_string().contains("partial durable run admission"));
    assert_eq!(
        store
            .protocol_event_store()
            .list_history_items_for_session(nested.id)
            .expect("history after rolled-back append")
            .len(),
        history_before
    );
    assert!(
        repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("requests after rolled-back append")
            .is_empty()
    );
}

#[tokio::test]
#[cfg(any())]
async fn lazy_upgrade_seed_restores_idle_nested_trigger_and_is_idempotent() {
    let (store, root_session_id) = test_repo().await;
    let owner = create_sibling_session(&store, root_session_id, "owner").await;
    let nested = create_sibling_session(&store, root_session_id, "nested").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            owner.id,
            "/root/owner",
            "owner",
        )
        .await
        .expect("owner edge");
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            nested.id,
            "/root/owner/nested",
            "nested",
        )
        .await
        .expect("nested edge");
    let trigger = repository
        .append_inter_agent_communication_with_protocol_bundle(
            nested.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner/nested".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/nested",
                    "/root",
                    "pre-V48 pending nested work",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("nested trigger");
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute("DELETE FROM agent_owner_resume_requests", [])
            .expect("simulate empty V48 upgrade table");
    }

    let seeded = repository
        .ensure_owner_resumes_for_pending_nested_triggers(root_session_id)
        .expect("lazy V48 seed");
    assert_eq!(
        seeded,
        vec![ScheduledOwnerResume {
            owner_session_id: owner.id,
            request_id: OwnerResumeRequestId::from(trigger.history_item_id),
        }]
    );
    assert_eq!(
        repository
            .ensure_owner_resumes_for_pending_nested_triggers(root_session_id)
            .expect("idempotent lazy V48 seed"),
        seeded
    );
    assert!(
        repository
            .list_pending_owner_resume_requests(root_session_id)
            .expect("root resume requests")
            .is_empty()
    );
}

#[tokio::test]
#[cfg(any())]
async fn newer_nested_followup_projects_the_owners_canonical_earliest_pending_resume() {
    let (store, root_session_id) = test_repo().await;
    let (owner, existing_requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let nested = create_sibling_session(&store, root_session_id, "new_nested").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            nested.id,
            "/root/owner/new_nested",
            "new_nested",
        )
        .await
        .expect("new nested edge");

    let stored = repository
        .append_inter_agent_communication_with_protocol_bundle(
            nested.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner/new_nested".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/new_nested",
                    "/root",
                    "newer nested follow-up",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("newer nested follow-up");

    assert_eq!(stored.scheduled_owner_resumes.len(), 1);
    assert_eq!(stored.scheduled_owner_resumes[0].owner_session_id, owner.id);
    assert_eq!(
        stored.scheduled_owner_resumes[0].request_id, existing_requests[0].request_id,
        "live projection must reserve the canonical oldest pending request"
    );
    assert_ne!(
        stored.scheduled_owner_resumes[0].request_id.to_string(),
        stored.history_item_id.to_string()
    );
    assert_eq!(
        repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("coalesced pending rows")
            .len(),
        2
    );
}

#[tokio::test]
async fn newer_completion_handoff_projects_the_parents_canonical_pending_resume() {
    let (store, root_session_id) = test_repo().await;
    let (owner, existing_requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let child = create_sibling_session(&store, root_session_id, "later_child").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            child.id,
            "/root/owner/later_child",
            "later_child",
        )
        .await
        .expect("later child edge");
    let (admission_id, turn_id) = active_turn(&store, child.id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                admission_id,
                &failed_terminal(child.id, "later child failed"),
                turn_id,
                None,
                None,
            )
            .await
            .expect("later child terminal"),
        AdmittedTerminalCommit::Applied
    );

    let handoff = repository
        .agent_completion_handoff(child.id, turn_id)
        .expect("later child handoff")
        .expect("failed child FINAL");
    assert_ne!(
        handoff.history_item_id.to_string(),
        existing_requests[0].request_id.to_string()
    );
    assert_eq!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("current handoff owner"),
        Some(existing_requests[0].request_id)
    );
    assert_eq!(
        repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("pending handoff requests")
            .len(),
        1
    );
}

#[tokio::test]
async fn late_child_handoff_is_preserved_for_failed_parent_without_owner_resume() {
    let (store, root_session_id) = test_repo().await;
    let owner = create_sibling_session(&store, root_session_id, "owner").await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            owner.id,
            "/root/owner",
            "owner",
        )
        .await
        .expect("owner edge");
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            child.id,
            "/root/owner/child",
            "child",
        )
        .await
        .expect("child edge");
    let (owner_admission_id, owner_turn_id) = active_turn(&store, owner.id).await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                owner_admission_id,
                &failed_terminal(owner.id, "owner failed explicitly"),
                owner_turn_id,
                None,
                None,
            )
            .await
            .expect("owner failure"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, None),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("late child completion"),
        AdmittedTerminalCommit::Applied
    );

    let handoff = repository
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("late child handoff")
        .expect("late child FINAL is retained");
    assert_eq!(handoff.parent_session_id, owner.id);
    assert!(
        repository
            .list_pending_owner_resume_requests(owner.id)
            .expect("failed parent owner resumes")
            .is_empty()
    );
}

#[tokio::test]
async fn child_handoff_during_explicit_owner_turn_does_not_reproject_claimed_resume() {
    let (store, root_session_id) = test_repo().await;
    let (owner, existing_requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let child = create_sibling_session(&store, root_session_id, "racing_child").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            owner.id,
            child.id,
            "/root/owner/racing_child",
            "racing_child",
        )
        .await
        .expect("racing child edge");
    let explicit = repository
        .append_inter_agent_communication_with_protocol_bundle(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "explicit owner turn",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("explicit owner trigger");
    let owner_turn_id = TurnId::new();
    repository
        .admit_agent_triggered_turn(owner.id, owner_turn_id, explicit.history_item_id)
        .await
        .expect("explicit owner admission")
        .expect("explicit owner admitted");
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, None),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("child completion during explicit owner"),
        AdmittedTerminalCommit::Applied
    );

    let _handoff = repository
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("racing child handoff")
        .expect("turn-scoped child FINAL");
    assert_eq!(
        repository
            .schedulable_owner_resume_request_id(owner.id)
            .expect("claimed request projection"),
        None
    );
    let repository_handle = store.session_repo();
    let connection = repository_handle.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT state
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                params![
                    owner.id.to_string(),
                    existing_requests[0].request_id.to_string()
                ],
                |row| row.get::<_, String>(0),
            )
            .expect("claimed old request"),
        "claimed"
    );
}

#[tokio::test]
async fn explicit_agent_trigger_admission_coalesces_pending_owner_resume_rows() {
    let (store, root_session_id) = test_repo().await;
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let explicit = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            owner.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "explicit owner work",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("explicit owner trigger");
    assert_eq!(
        store
            .session_repo()
            .schedulable_owner_resume_request_id(owner.id)
            .expect("explicit precedence projection"),
        None
    );
    let turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_agent_triggered_turn(owner.id, turn_id, explicit.history_item_id)
        .await
        .expect("explicit admission")
        .expect("explicit admitted");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                admission.admission_id,
                turn_id,
                128,
            )
            .expect("safe explicit-turn mailbox delivery")
            .history_item_ids,
        vec![requests[0].request_id.0, explicit.history_item_id],
        "the explicit admission coalesces the OwnerResume row, while its safe boundary samples both queued inputs in enqueue order"
    );
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT state || ':' || claimed_turn_id
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                params![owner.id.to_string(), requests[0].request_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .expect("coalesced explicit state"),
        format!("claimed:{turn_id}")
    );
    drop(connection);
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                owner.id,
                admission.admission_id,
                &completed_terminal_for_response(owner.id, None),
                turn_id,
                None,
                None,
            )
            .await
            .expect("explicit terminal"),
        AdmittedTerminalCommit::Applied
    );
}

#[tokio::test]
async fn two_connections_cannot_claim_one_owner_resume_request_twice() {
    let (store, root_session_id) = test_repo().await;
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let reopened_sqlite = SqliteStore::open(store.paths()).expect("second sqlite connection");
    reopened_sqlite
        .migrate()
        .expect("migrate second connection");
    let competitor = StoreBundle::new(reopened_sqlite);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_turn = TurnId::new();
    let second_turn = TurnId::new();
    let request_id = requests[0].request_id;
    let (first, second) = std::thread::scope(|scope| {
        let first_barrier = barrier.clone();
        let first_store = &store;
        let first_thread = scope.spawn(move || {
            first_barrier.wait();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("first runtime")
                .block_on(
                    first_store
                        .session_repo()
                        .admit_owner_resume_turn(owner.id, first_turn, request_id),
                )
        });
        let second_barrier = barrier.clone();
        let second_store = &competitor;
        let second_thread = scope.spawn(move || {
            second_barrier.wait();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("second runtime")
                .block_on(second_store.session_repo().admit_owner_resume_turn(
                    owner.id,
                    second_turn,
                    request_id,
                ))
        });
        (
            first_thread.join().expect("first admission"),
            second_thread.join().expect("second admission"),
        )
    });
    let first = first.expect("first result");
    let second = second.expect("second result");
    assert_ne!(first.is_some(), second.is_some());
    let winning_turn = if first.is_some() {
        first_turn
    } else {
        second_turn
    };
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT claimed_turn_id
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                params![owner.id.to_string(), request_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .expect("winning owner-resume claim"),
        winning_turn.to_string()
    );
}

#[tokio::test]
async fn crashed_owner_resume_turn_repends_once_and_suppresses_crash_final() {
    let (store, root_session_id) = test_repo().await;
    let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
    let owner_turn_id = TurnId::new();
    let owner_admission = store
        .session_repo()
        .admit_owner_resume_turn(owner.id, owner_turn_id, requests[0].request_id)
        .await
        .expect("owner-resume admission")
        .expect("owner resume admitted");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                owner.id,
                owner_admission.admission_id,
                owner_turn_id,
                128,
            )
            .expect("safe OwnerResume delivery before crash")
            .history_item_ids,
        vec![requests[0].request_id.0]
    );
    let target = store
        .session_repo()
        .captured_running_terminal_target(owner.id)
        .await
        .expect("capture owner")
        .expect("running owner");

    assert!(
        store
            .session_repo()
            .recover_captured_running_session_with_protocol_event(
                owner.id,
                &failed_terminal(owner.id, "owner process crashed"),
                target,
            )
            .await
            .expect("recover owner")
    );
    assert_eq!(
        store
            .session_repo()
            .schedulable_owner_resume_request_id(owner.id)
            .expect("re-pended request"),
        Some(requests[0].request_id)
    );
    assert!(
        store
            .session_repo()
            .agent_completion_handoff(owner.id, owner_turn_id)
            .expect("suppressed crash handoff query")
            .is_none()
    );
    assert!(
        !store
            .session_repo()
            .recover_captured_running_session_with_protocol_event(
                owner.id,
                &failed_terminal(owner.id, "duplicate recovery"),
                target,
            )
            .await
            .expect("idempotent retry")
    );
    assert_eq!(
        store
            .protocol_event_store()
            .list_runtime_events(owner.id, owner_turn_id)
            .expect("owner recovery events")
            .into_iter()
            .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
            .count(),
        1
    );
    assert_eq!(
        store
            .session_repo()
            .pending_deferred_completion(owner.id)
            .expect("pending crash deferred")
            .expect("recoverable crash receipt")
            .state,
        DeferredAgentCompletionState::Pending
    );
    assert!(matches!(
        store
            .session_repo()
            .settle_pending_owner_resume_with_terminal(
                owner.id,
                requests[0].request_id,
                pre_admission_interrupted_terminal(),
            )
            .expect("stop pending recovered OwnerResume"),
        PendingAgentTriggerSettlement::Applied { handoff: None, .. }
    ));
    assert_eq!(
        store
            .session_repo()
            .agent_terminal_effects(owner.id, owner_turn_id)
            .expect("discarded crash effects")
            .deferred
            .expect("discarded crash receipt")
            .state,
        DeferredAgentCompletionState::Discarded
    );
    assert!(
        store
            .session_repo()
            .schedulable_owner_resume_request_id(owner.id)
            .expect("stopped OwnerResume")
            .is_none()
    );
}

#[tokio::test]
async fn agent_spawn_commits_edge_context_boundary_and_initial_task_atomically() {
    let (store, root_session_id) = test_repo().await;
    let child_session_id = SessionId::new();
    let child_draft = sibling_session_draft(&store, root_session_id, "child").await;
    let rolled_back_child_session_id = SessionId::new();
    let rolled_back_child_draft =
        sibling_session_draft(&store, root_session_id, "rolled_back").await;
    let root_turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_session_turn(root_session_id, root_turn_id)
        .await
        .expect("root admission")
        .expect("root admitted");

    let stored = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                child_session_id,
                child_draft,
                "/root/child",
                "child",
                admission.admission_id,
                root_turn_id,
                SpawnContextFork::None,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: "Message Type: NEW_TASK\nTask name: /root/child\nSender: /root\nPayload:\nDo the bounded task.".to_string(),
                    trigger_turn: true,
                },
            )
            .expect("atomic child spawn");
    assert_eq!(stored.child_session.id, child_session_id);
    assert_eq!(stored.child_session.title, "child");
    assert_eq!(stored.edge.spawn_order, 1);
    let child_history = store
        .protocol_event_store()
        .list_history_items_for_session(child_session_id)
        .expect("child history");
    assert!(child_history.is_empty());
    let queued_initial_task = store
        .session_repo()
        .agent_mailbox_communications_by_id(
            child_session_id,
            &[stored.initial_task_history_item_id],
        )
        .expect("queued initial task");
    assert!(matches!(
        queued_initial_task.as_slice(),
        [(id, communication)]
            if *id == stored.initial_task_history_item_id
                && communication.trigger_turn
                && communication.author == "/root"
                && communication.recipient == "/root/child"
    ));
    let pending_before_admission = store
        .protocol_event_store()
        .retained_descendant_page(root_session_id, 0, 10)
        .expect("retained child projection");
    assert_eq!(
        pending_before_admission.items[0].pending_trigger_history_item_id,
        Some(stored.initial_task_history_item_id)
    );
    let child_turn_id = TurnId::new();
    let child_admission = store
        .session_repo()
        .admit_agent_triggered_turn(
            child_session_id,
            child_turn_id,
            stored.initial_task_history_item_id,
        )
        .await
        .expect("child trigger admission")
        .expect("child trigger admitted");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                child_session_id,
                child_admission.admission_id,
                child_turn_id,
                128,
            )
            .expect("safe initial-task delivery")
            .history_item_ids,
        vec![stored.initial_task_history_item_id]
    );
    let claimed_after_admission = store
        .protocol_event_store()
        .retained_descendant_page(root_session_id, 0, 10)
        .expect("claimed child projection");
    assert_eq!(
        claimed_after_admission.items[0].pending_trigger_history_item_id, None,
        "the atomic SessionStarted append must durably claim earlier session mail"
    );
    let delivered_initial_task = store
        .protocol_event_store()
        .history_items_by_id(child_session_id, &[stored.initial_task_history_item_id])
        .expect("delivered initial task");
    assert!(matches!(
        delivered_initial_task.as_slice(),
        [HistoryItem {
            scope: HistoryScope::Turn { turn_id },
            payload: HistoryItemPayload::InterAgentCommunication { communication },
            ..
        }] if *turn_id == child_turn_id
            && communication.trigger_turn
            && communication.author == "/root"
            && communication.recipient == "/root/child"
    ));

    let error = store
        .session_repo()
        .create_agent_spawn_with_initial_task_for_caller_turn(
            root_session_id,
            root_session_id,
            rolled_back_child_session_id,
            rolled_back_child_draft,
            "/root/rolled_back",
            "rolled_back",
            admission.admission_id,
            root_turn_id,
            SpawnContextFork::Recent(0),
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/rolled_back".to_string(),
                content: "must roll back".to_string(),
                trigger_turn: true,
            },
        )
        .expect_err("invalid fork must roll back the whole durable spawn");
    assert!(error.to_string().contains("requires at least one turn"));
    let edges = store
        .session_repo()
        .list_session_spawn_edges(root_session_id)
        .await
        .expect("retained edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].child_session_id, child_session_id);
    assert!(
        store
            .session_repo()
            .get_session(rolled_back_child_session_id)
            .await
            .is_err(),
        "context fork failure must roll back the child session row"
    );
}

async fn assert_spawn_rolled_back(
    store: &StoreBundle,
    root_session_id: SessionId,
    child_session_id: SessionId,
) {
    assert!(
        store
            .session_repo()
            .get_session(child_session_id)
            .await
            .is_err(),
        "failed spawn must not retain a child session row"
    );
    assert!(
        store
            .session_repo()
            .list_session_spawn_edges(root_session_id)
            .await
            .expect("spawn edges")
            .iter()
            .all(|edge| edge.child_session_id != child_session_id),
        "failed spawn must not retain an edge"
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(child_session_id)
            .expect("child history")
            .is_empty(),
        "failed spawn must not retain child history"
    );
}

#[tokio::test]
async fn child_session_edge_fork_and_initial_task_failures_roll_back_one_spawn_transaction() {
    let (store, root_session_id) = test_repo().await;
    let root_turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_session_turn(root_session_id, root_turn_id)
        .await
        .expect("root admission")
        .expect("root admitted");

    let cases = [
        (
            "session_insert",
            "CREATE TEMP TRIGGER injected_agent_session_failure
                 BEFORE INSERT ON sessions
                 BEGIN
                     SELECT RAISE(ABORT, 'injected child session failure');
                 END;",
            "DROP TRIGGER injected_agent_session_failure;",
            SpawnContextFork::None,
        ),
        (
            "edge_insert",
            "CREATE TEMP TRIGGER injected_agent_edge_failure
                 BEFORE INSERT ON session_spawn_edges
                 BEGIN
                     SELECT RAISE(ABORT, 'injected child edge failure');
                 END;",
            "DROP TRIGGER injected_agent_edge_failure;",
            SpawnContextFork::None,
        ),
        (
            "initial_task",
            "CREATE TEMP TRIGGER injected_agent_task_failure
                 BEFORE INSERT ON agent_mailbox_messages
                 BEGIN
                     SELECT RAISE(ABORT, 'injected initial task failure');
                 END;",
            "DROP TRIGGER injected_agent_task_failure;",
            SpawnContextFork::None,
        ),
    ];
    for (task_name, install_trigger, drop_trigger, context_fork) in cases {
        let child_session_id = SessionId::new();
        let child_draft = sibling_session_draft(&store, root_session_id, task_name).await;
        let child_path = format!("/root/{task_name}");
        {
            let repository = store.session_repo();
            repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .execute_batch(install_trigger)
                .expect("install injected failure");
        }
        let error = store
            .session_repo()
            .create_agent_spawn_with_initial_task_for_caller_turn(
                root_session_id,
                root_session_id,
                child_session_id,
                child_draft,
                &child_path,
                task_name,
                admission.admission_id,
                root_turn_id,
                context_fork,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: child_path.clone(),
                    content: format!("run {task_name}"),
                    trigger_turn: true,
                },
            )
            .expect_err("injected storage failure must abort the spawn");
        assert!(error.to_string().contains("injected"));
        {
            let repository = store.session_repo();
            repository
                .connection
                .lock()
                .expect("sqlite mutex")
                .execute_batch(drop_trigger)
                .expect("remove injected failure");
        }
        assert_spawn_rolled_back(&store, root_session_id, child_session_id).await;
    }

    let fork_child_session_id = SessionId::new();
    let fork_child_draft = sibling_session_draft(&store, root_session_id, "fork_failure").await;
    let error = store
        .session_repo()
        .create_agent_spawn_with_initial_task_for_caller_turn(
            root_session_id,
            root_session_id,
            fork_child_session_id,
            fork_child_draft,
            "/root/fork_failure",
            "fork_failure",
            admission.admission_id,
            root_turn_id,
            SpawnContextFork::Recent(0),
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/fork_failure".to_string(),
                content: "invalid recent fork".to_string(),
                trigger_turn: true,
            },
        )
        .expect_err("invalid context fork must abort the spawn");
    assert!(error.to_string().contains("requires at least one turn"));
    assert_spawn_rolled_back(&store, root_session_id, fork_child_session_id).await;

    let stale_child_session_id = SessionId::new();
    let stale_child_draft = sibling_session_draft(&store, root_session_id, "stale_owner").await;
    let error = store
        .session_repo()
        .create_agent_spawn_with_initial_task_for_caller_turn(
            root_session_id,
            root_session_id,
            stale_child_session_id,
            stale_child_draft,
            "/root/stale_owner",
            "stale_owner",
            AdmissionId::new(),
            root_turn_id,
            SpawnContextFork::None,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/stale_owner".to_string(),
                content: "stale caller".to_string(),
                trigger_turn: true,
            },
        )
        .expect_err("stale caller admission must abort before child creation");
    assert!(error.to_string().contains("no longer owns active turn"));
    assert_spawn_rolled_back(&store, root_session_id, stale_child_session_id).await;
}

#[tokio::test]
async fn nested_agent_spawn_binds_initial_task_to_the_immediate_parent_path() {
    let (store, root_session_id) = test_repo().await;
    let root_turn_id = TurnId::new();
    let root_admission = store
        .session_repo()
        .admit_session_turn(root_session_id, root_turn_id)
        .await
        .expect("root admission")
        .expect("root admitted");
    let parent_session_id = SessionId::new();
    let parent_draft = sibling_session_draft(&store, root_session_id, "parent").await;
    store
        .session_repo()
        .create_agent_spawn_with_initial_task_for_caller_turn(
            root_session_id,
            root_session_id,
            parent_session_id,
            parent_draft,
            "/root/parent",
            "parent",
            root_admission.admission_id,
            root_turn_id,
            SpawnContextFork::None,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/parent".to_string(),
                content: "parent task".to_string(),
                trigger_turn: true,
            },
        )
        .expect("parent spawn");
    let parent_turn_id = TurnId::new();
    let parent_admission = store
        .session_repo()
        .admit_session_turn(parent_session_id, parent_turn_id)
        .await
        .expect("parent admission")
        .expect("parent admitted");

    let child_session_id = SessionId::new();
    let child_draft = sibling_session_draft(&store, root_session_id, "child").await;
    let stored = store
        .session_repo()
        .create_agent_spawn_with_initial_task_for_caller_turn(
            root_session_id,
            parent_session_id,
            child_session_id,
            child_draft,
            "/root/parent/child",
            "child",
            parent_admission.admission_id,
            parent_turn_id,
            SpawnContextFork::None,
            InterAgentCommunication {
                author: "/root/parent".to_string(),
                recipient: "/root/parent/child".to_string(),
                content: "nested task".to_string(),
                trigger_turn: true,
            },
        )
        .expect("nested spawn");
    assert_eq!(stored.edge.parent_session_id, parent_session_id);
    assert_eq!(stored.edge.agent_path, "/root/parent/child");
    let child_history = store
        .protocol_event_store()
        .list_history_items_for_session(child_session_id)
        .expect("nested child history");
    assert!(child_history.is_empty());
    let queued_initial_task = store
        .session_repo()
        .agent_mailbox_communications_by_id(
            child_session_id,
            &[stored.initial_task_history_item_id],
        )
        .expect("nested queued initial task");
    assert!(matches!(
        queued_initial_task.as_slice(),
        [(id, communication)] if *id == stored.initial_task_history_item_id
            && communication.author == "/root/parent"
            && communication.recipient == "/root/parent/child"
            && communication.trigger_turn
    ));
    let child_turn_id = TurnId::new();
    let child_admission = store
        .session_repo()
        .admit_agent_triggered_turn(
            child_session_id,
            child_turn_id,
            stored.initial_task_history_item_id,
        )
        .await
        .expect("nested child trigger admission")
        .expect("nested child admitted");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                child_session_id,
                child_admission.admission_id,
                child_turn_id,
                128,
            )
            .expect("nested safe initial-task delivery")
            .history_item_ids,
        vec![stored.initial_task_history_item_id]
    );

    let wrong_author_session_id = SessionId::new();
    let wrong_author_draft = sibling_session_draft(&store, root_session_id, "wrong_author").await;
    let error = store
        .session_repo()
        .create_agent_spawn_with_initial_task_for_caller_turn(
            root_session_id,
            parent_session_id,
            wrong_author_session_id,
            wrong_author_draft,
            "/root/parent/wrong_author",
            "wrong_author",
            parent_admission.admission_id,
            parent_turn_id,
            SpawnContextFork::None,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/parent/wrong_author".to_string(),
                content: "wrong sender".to_string(),
                trigger_turn: true,
            },
        )
        .expect_err("nested initial task must use its immediate parent author");
    assert!(error.to_string().contains("immediate parent"));
    assert_spawn_rolled_back(&store, root_session_id, wrong_author_session_id).await;
}

#[tokio::test]
async fn spawn_edge_repository_accepts_recursive_lineage_and_rejects_invalid_parentage() {
    let (store, root_session_id) = test_repo().await;
    let direct = create_sibling_session(&store, root_session_id, "direct").await;
    let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
    let wrong_path = create_sibling_session(&store, root_session_id, "wrong_path").await;
    let unattached_parent =
        create_sibling_session(&store, root_session_id, "unattached_parent").await;
    let unattached_child =
        create_sibling_session(&store, root_session_id, "unattached_child").await;
    let other_root = create_sibling_session(&store, root_session_id, "other_root").await;
    let other_parent = create_sibling_session(&store, root_session_id, "other_parent").await;
    let cross_tree_child =
        create_sibling_session(&store, root_session_id, "cross_tree_child").await;
    let invalid_name = create_sibling_session(&store, root_session_id, "invalid_name").await;
    let repository = store.session_repo();

    let direct_edge = repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            direct.id,
            "/root/direct",
            "direct",
        )
        .await
        .expect("direct edge");
    let grandchild_edge = repository
        .insert_session_spawn_edge(
            root_session_id,
            direct.id,
            grandchild.id,
            "/root/direct/grandchild",
            "grandchild",
        )
        .await
        .expect("nested edge");
    repository
        .insert_session_spawn_edge(
            other_root.id,
            other_root.id,
            other_parent.id,
            "/root/other_parent",
            "other_parent",
        )
        .await
        .expect("other tree parent");

    assert_eq!(direct_edge.parent_session_id, root_session_id);
    assert_eq!(direct_edge.agent_path, "/root/direct");
    assert_eq!(grandchild_edge.parent_session_id, direct.id);
    assert_eq!(grandchild_edge.agent_path, "/root/direct/grandchild");
    assert_eq!(
        repository
            .session_spawn_edge_for_child(grandchild.id)
            .await
            .expect("grandchild lookup"),
        Some(grandchild_edge.clone())
    );

    let cases = [
        (
            direct.id,
            wrong_path.id,
            "/root/wrong_path",
            "wrong_path",
            "does not match canonical parent/task path",
        ),
        (
            unattached_parent.id,
            unattached_child.id,
            "/root/unattached_parent/unattached_child",
            "unattached_child",
            "is not a retained agent in root tree",
        ),
        (
            other_parent.id,
            cross_tree_child.id,
            "/root/other_parent/cross_tree_child",
            "cross_tree_child",
            "is not a retained agent in root tree",
        ),
        (
            root_session_id,
            invalid_name.id,
            "/root/BadName",
            "BadName",
            "invalid task name",
        ),
        (
            root_session_id,
            root_session_id,
            "/root/self",
            "self",
            "cannot also be its own child",
        ),
    ];
    for (parent_session_id, child_session_id, path, task_name, expected) in cases {
        let error = repository
            .insert_session_spawn_edge(
                root_session_id,
                parent_session_id,
                child_session_id,
                path,
                task_name,
            )
            .await
            .expect_err("invalid edge must fail before SQLite mutation");
        assert!(
            error.to_string().contains(expected),
            "unexpected error: {error}"
        );
    }

    assert_eq!(
        repository
            .list_session_spawn_edges(root_session_id)
            .await
            .expect("recursive edges"),
        vec![direct_edge.clone(), grandchild_edge.clone()]
    );
    assert_eq!(
        repository
            .list_session_subtree_ids(root_session_id)
            .await
            .expect("root subtree"),
        vec![root_session_id, direct.id, grandchild.id]
    );
    assert_eq!(
        repository
            .list_session_subtree_ids(direct.id)
            .await
            .expect("nested subtree"),
        vec![direct.id, grandchild.id]
    );
    let initial_states = repository
        .list_descendant_run_admission_states(root_session_id)
        .await
        .expect("initial descendant states");
    assert_eq!(initial_states.len(), 2);
    assert!(
        initial_states
            .iter()
            .all(|state| !state.blocks_new_root_turn)
    );
    repository
        .admit_session_turn(grandchild.id, TurnId::new())
        .await
        .expect("grandchild admission")
        .expect("grandchild admitted");
    let admitted_states = repository
        .list_descendant_run_admission_states(root_session_id)
        .await
        .expect("admitted descendant states");
    assert_eq!(admitted_states.len(), 2);
    assert!(
        admitted_states.iter().any(|state| {
            state.edge.child_session_id == grandchild.id && state.blocks_new_root_turn
        }),
        "a running grandchild must block a new root turn"
    );
    assert!(
        admitted_states.iter().any(|state| {
            state.edge.child_session_id == direct.id && !state.blocks_new_root_turn
        }),
        "an idle direct parent must remain non-blocking"
    );
    assert!(
        repository.get_session(cross_tree_child.id).await.is_ok(),
        "a rejected cross-tree edge must not delete the independent session"
    );
}

#[tokio::test]
async fn spawn_edge_repository_enforces_one_project_and_one_tree_owner_per_session() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    let nested_candidate =
        create_sibling_session(&store, root_session_id, "nested_candidate").await;
    let root_child_candidate =
        create_sibling_session(&store, root_session_id, "root_child_candidate").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("owned child");

    let child_as_root = repository
        .insert_session_spawn_edge(
            child.id,
            child.id,
            nested_candidate.id,
            "/root/nested_candidate",
            "nested_candidate",
        )
        .await
        .expect_err("a retained child cannot own a second tree");
    assert!(child_as_root.to_string().contains("retained descendant"));

    let existing_root_as_child = repository
        .insert_session_spawn_edge(
            root_child_candidate.id,
            root_child_candidate.id,
            root_session_id,
            "/root/root",
            "root",
        )
        .await
        .expect_err("an existing tree root cannot become a child");
    assert!(
        existing_root_as_child
            .to_string()
            .contains("invalid task name")
            || existing_root_as_child
                .to_string()
                .contains("already owns an agent tree")
    );

    let root = repository
        .get_session(root_session_id)
        .await
        .expect("root session");
    let foreign_project_id = ProjectId::new();
    let foreign_root = root.cwd.join("foreign-project");
    store
        .project_repo()
        .upsert_project(foreign_project_id, &foreign_root, "foreign project", "none")
        .await
        .expect("foreign project");
    let foreign_child = repository
        .create_session(NewSession {
            project_id: foreign_project_id,
            title: "foreign child".to_string(),
            cwd: foreign_root,
            model: root.model,
            base_url: root.base_url,
            access_mode: root.access_mode,
            provider_connection: root.provider_connection,
        })
        .await
        .expect("foreign child");
    let cross_project = repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            foreign_child.id,
            "/root/foreign_child",
            "foreign_child",
        )
        .await
        .expect_err("cross-project child must be rejected");
    assert!(cross_project.to_string().contains("belong to one project"));
}

#[tokio::test]
async fn deleting_a_nested_subtree_preserves_its_ancestor_and_sibling() {
    let (store, root_session_id) = test_repo().await;
    let parent = create_sibling_session(&store, root_session_id, "parent").await;
    let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
    let sibling = create_sibling_session(&store, root_session_id, "sibling").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            parent.id,
            "/root/parent",
            "parent",
        )
        .await
        .expect("parent edge");
    repository
        .insert_session_spawn_edge(
            root_session_id,
            parent.id,
            grandchild.id,
            "/root/parent/grandchild",
            "grandchild",
        )
        .await
        .expect("grandchild edge");
    let sibling_edge = repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            sibling.id,
            "/root/sibling",
            "sibling",
        )
        .await
        .expect("sibling edge");

    let deleted = repository
        .delete_session_tree(parent.id)
        .await
        .expect("nested subtree delete");

    assert_eq!(deleted, vec![grandchild.id, parent.id]);
    assert!(repository.get_session(root_session_id).await.is_ok());
    assert!(repository.get_session(sibling.id).await.is_ok());
    assert!(repository.get_session(parent.id).await.is_err());
    assert!(repository.get_session(grandchild.id).await.is_err());
    assert_eq!(
        repository
            .list_session_spawn_edges(root_session_id)
            .await
            .expect("remaining tree"),
        vec![sibling_edge]
    );
}

#[tokio::test]
async fn deleting_a_root_tree_removes_delivered_recipient_mail_before_history() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let stored = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: "delivered before root-tree deletion".to_string(),
                trigger_turn: false,
            },
            true,
        )
        .expect("pending child mail");
    let delivered = store
        .session_repo()
        .deliver_pending_agent_mail_for_admitted_turn(
            child.id,
            child_admission_id,
            child_turn_id,
            128,
        )
        .expect("deliver child mail");
    assert_eq!(delivered.history_item_ids, vec![stored.history_item_id]);
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &agent_interrupted_terminal(child.id),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("close child turn"),
        AdmittedTerminalCommit::Applied
    );

    let deleted = store
        .session_repo()
        .delete_session_tree(root_session_id)
        .await
        .expect("delete complete root tree");
    assert_eq!(deleted, vec![child.id, root_session_id]);
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM agent_mailbox_messages", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("mailbox count"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM protocol_history_items", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("history count"),
        0
    );
}

#[tokio::test]
async fn deleting_a_nested_author_retains_delivered_owner_mail_with_tombstoned_author() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let stored = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            root_session_id,
            InterAgentCommunication {
                author: "/root/child".to_string(),
                recipient: "/root".to_string(),
                content: "completed evidence retained by owner".to_string(),
                trigger_turn: false,
            },
            false,
        )
        .expect("pending owner mail");
    let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
    let delivered = store
        .session_repo()
        .deliver_pending_agent_mail_for_admitted_turn(
            root_session_id,
            root_admission_id,
            root_turn_id,
            128,
        )
        .expect("deliver owner mail");
    assert_eq!(delivered.history_item_ids, vec![stored.history_item_id]);
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                root_admission_id,
                &agent_interrupted_terminal(root_session_id),
                root_turn_id,
                None,
                None,
            )
            .await
            .expect("close owner turn"),
        AdmittedTerminalCommit::Applied
    );

    assert_eq!(
        store
            .session_repo()
            .delete_session_tree(child.id)
            .await
            .expect("delete completed child"),
        vec![child.id]
    );
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    let retained = connection
        .query_row(
            "SELECT author_session_id, recipient_session_id, state
                 FROM agent_mailbox_messages
                 WHERE id = ?1",
            params![stored.history_item_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .expect("retained owner mailbox");
    assert_eq!(retained.0, None);
    assert_eq!(retained.1, root_session_id.to_string());
    assert_eq!(retained.2, "delivered");
    assert!(
        connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1
                     FROM protocol_history_items
                     WHERE id = ?1 AND session_id = ?2
                 )",
                params![
                    stored.history_item_id.to_string(),
                    root_session_id.to_string()
                ],
                |row| row.get::<_, bool>(0),
            )
            .expect("retained owner history")
    );
}

#[tokio::test]
async fn deleting_a_nested_author_rejects_pending_mail_owned_by_ancestor() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let stored = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            root_session_id,
            InterAgentCommunication {
                author: "/root/child".to_string(),
                recipient: "/root".to_string(),
                content: "owner has not observed this yet".to_string(),
                trigger_turn: false,
            },
            false,
        )
        .expect("pending owner mail");

    let error = store
        .session_repo()
        .delete_session_tree(child.id)
        .await
        .expect_err("pending outgoing owner mail must block author deletion");
    assert!(error.to_string().contains("pending owner mail exists"));
    assert!(
        error
            .to_string()
            .contains(&stored.history_item_id.to_string())
    );
    assert!(store.session_repo().get_session(child.id).await.is_ok());
    assert_eq!(
        store
            .session_repo()
            .agent_mailbox_communications_by_id(root_session_id, &[stored.history_item_id],)
            .expect("pending owner mailbox")
            .len(),
        1
    );
}

#[tokio::test]
async fn descendant_tree_mutation_is_blocked_by_its_active_ancestor_owner() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    repository
        .admit_session_turn(root_session_id, TurnId::new())
        .await
        .expect("root admission")
        .expect("root admitted");

    assert_eq!(
        repository
            .mutation_blocker_in_session_tree(child.id)
            .await
            .expect("descendant mutation blocker"),
        Some(root_session_id)
    );
    let archive_error = repository
        .set_session_archived(child.id, true)
        .await
        .expect_err("active ancestor must block descendant archive");
    assert!(
        archive_error
            .to_string()
            .contains(&root_session_id.to_string())
    );
    let delete_error = repository
        .delete_session_tree(child.id)
        .await
        .expect_err("active ancestor must block descendant delete");
    assert!(
        delete_error
            .to_string()
            .contains(&root_session_id.to_string())
    );
    assert!(repository.get_session(child.id).await.is_ok());
}

async fn assert_pending_trigger_blocks_destructive_mutations(
    store: &StoreBundle,
    root_session_id: SessionId,
    child_session_id: SessionId,
    project_id: ProjectId,
) {
    let repository = store.session_repo();
    assert_eq!(
        repository
            .mutation_blocker_in_session_tree(root_session_id)
            .await
            .expect("root mutation blocker"),
        Some(child_session_id)
    );
    assert_eq!(
        repository
            .mutation_blocker_in_session_tree(child_session_id)
            .await
            .expect("descendant mutation blocker"),
        Some(child_session_id)
    );
    let rollback_error = repository
        .rollback_session_transaction(root_session_id, 1)
        .await
        .expect_err("pending trigger must block root rollback");
    assert!(
        rollback_error
            .to_string()
            .contains(&child_session_id.to_string())
    );
    let archive_error = repository
        .set_session_archived(child_session_id, true)
        .await
        .expect_err("pending trigger must block descendant archive");
    assert!(
        archive_error
            .to_string()
            .contains(&child_session_id.to_string())
    );
    let delete_error = repository
        .delete_session_tree(child_session_id)
        .await
        .expect_err("pending trigger must block descendant delete");
    assert!(
        delete_error
            .to_string()
            .contains(&child_session_id.to_string())
    );
    let project_error = store
        .project_repo()
        .delete_project(project_id)
        .await
        .expect_err("pending trigger must block project delete");
    assert!(
        project_error
            .to_string()
            .contains(&child_session_id.to_string())
    );
    assert!(repository.get_session(root_session_id).await.is_ok());
    assert!(repository.get_session(child_session_id).await.is_ok());
}

#[tokio::test]
async fn pending_trigger_blocks_destructive_mutation_before_and_after_store_restart() {
    let (store, root_session_id) = test_repo().await;
    let root = store
        .session_repo()
        .get_session(root_session_id)
        .await
        .expect("root session");
    let child = create_sibling_session(&store, root_session_id, "pending").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/pending",
            "pending",
        )
        .await
        .expect("pending child edge");
    let stored = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/pending".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/pending",
                    "/root",
                    "run after restart",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("pending child trigger");
    assert_eq!(
        store
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending trigger"),
        Some(stored.history_item_id)
    );

    assert_pending_trigger_blocks_destructive_mutations(
        &store,
        root_session_id,
        child.id,
        root.project_id,
    )
    .await;

    let paths = store.paths().clone();
    drop(store);
    let reopened_sqlite = SqliteStore::open(&paths).expect("reopen store");
    reopened_sqlite.migrate().expect("migrate reopened store");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert_pending_trigger_blocks_destructive_mutations(
        &reopened,
        root_session_id,
        child.id,
        root.project_id,
    )
    .await;
}

#[tokio::test]
async fn repository_mutations_fail_closed_for_durable_active_tree_state() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "active_child").await;
    let repository = store.session_repo();
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/active_child",
            "active_child",
        )
        .await
        .expect("child edge");
    repository
        .admit_session_turn(child.id, TurnId::new())
        .await
        .expect("child admission")
        .expect("child admitted");

    let archive_error = repository
        .set_session_archived(root_session_id, true)
        .await
        .expect_err("active child must block repository-level root archive");
    assert!(archive_error.to_string().contains(&child.id.to_string()));
    let delete_error = repository
        .delete_session_tree(root_session_id)
        .await
        .expect_err("active child must block repository-level tree delete");
    assert!(delete_error.to_string().contains(&child.id.to_string()));
    let settings_error = repository
        .update_session_settings(
            child.id,
            &SessionSettingsPatch {
                model: Some("changed-model".to_string()),
                ..SessionSettingsPatch::default()
            },
        )
        .await
        .expect_err("active target must block repository-level settings mutation");
    assert!(settings_error.to_string().contains("active"));
    assert!(repository.get_session(root_session_id).await.is_ok());
    assert!(repository.get_session(child.id).await.is_ok());
}

#[tokio::test]
async fn runtime_state_contract_rejects_partial_and_impossible_owners_across_readers() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let project_id = repository
        .get_session(session_id)
        .await
        .expect("initial session")
        .project_id;
    let admission_id = AdmissionId::new();
    let turn_id = TurnId::new();
    let lease_expires_at_ms = normalize_run_lease_now_ms(SystemClock::now_ms()) + 60_000;
    let admission_id_text = admission_id.to_string();
    let turn_id_text = turn_id.to_string();
    let cases = [
        ("running without owner", "running", None, None, None),
        (
            "running partial owner",
            "running",
            Some(admission_id_text.as_str()),
            None,
            Some(lease_expires_at_ms),
        ),
        (
            "terminal partial owner",
            "completed",
            None,
            Some(turn_id_text.as_str()),
            Some(lease_expires_at_ms),
        ),
        (
            "idle retained owner",
            "idle",
            Some(admission_id_text.as_str()),
            Some(turn_id_text.as_str()),
            Some(lease_expires_at_ms),
        ),
        (
            "invalid admission identity",
            "running",
            Some("not-an-admission"),
            Some(turn_id_text.as_str()),
            Some(lease_expires_at_ms),
        ),
        (
            "invalid turn identity",
            "running",
            Some(admission_id_text.as_str()),
            Some("not-a-turn"),
            Some(lease_expires_at_ms),
        ),
        (
            "nonpositive lease",
            "running",
            Some(admission_id_text.as_str()),
            Some(turn_id_text.as_str()),
            Some(0),
        ),
    ];

    for (label, status, active_run_id, active_turn_id, lease_expires_at_ms) in cases {
        {
            let connection = repository.connection.lock().expect("sqlite mutex poisoned");
            connection
                .execute(
                    "UPDATE sessions
                         SET status = ?2,
                             active_run_id = ?3,
                             active_turn_id = ?4,
                             active_run_lease_expires_at_ms = ?5
                         WHERE id = ?1",
                    params![
                        session_id.to_string(),
                        status,
                        active_run_id,
                        active_turn_id,
                        lease_expires_at_ms,
                    ],
                )
                .expect("inject invalid runtime state");
        }

        assert!(repository.get_session(session_id).await.is_err(), "{label}");
        assert!(
            repository.latest_session(project_id).await.is_err(),
            "{label}"
        );
        assert!(
            repository
                .session_projection_state(session_id)
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository.list_sessions(project_id, 10).await.is_err(),
            "{label}"
        );
        assert!(
            repository
                .has_fresh_run_admission(session_id)
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository
                .fresh_running_turn_for_session(session_id)
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository
                .session_blocks_mutation(session_id)
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository
                .mutation_blocker_in_session_tree(session_id)
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository
                .active_session_for_project(project_id)
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository
                .admitted_run_status_at(session_id, admission_id, turn_id, SystemClock::now_ms(),)
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository
                .renew_admitted_run_lease_at(
                    session_id,
                    admission_id,
                    turn_id,
                    SystemClock::now_ms(),
                    RUN_ADMISSION_LEASE_DURATION_MS,
                )
                .await
                .is_err(),
            "{label}"
        );
        assert!(
            repository
                .release_stopped_run_admission(session_id, admission_id)
                .await
                .is_err(),
            "{label}"
        );
    }
}

#[tokio::test]
async fn project_and_tree_gates_validate_later_corrupt_rows_after_a_valid_blocker() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let project_id = repository
        .get_session(root_session_id)
        .await
        .expect("root session")
        .project_id;
    let child = create_sibling_session(&store, root_session_id, "corrupt_child").await;
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/corrupt_child",
            "corrupt_child",
        )
        .await
        .expect("child edge");
    let corrupt_admission_id = AdmissionId::new().to_string();
    repository
        .inject_raw_runtime_state_for_corruption_test(
            child.id,
            "completed",
            Some(&corrupt_admission_id),
            None,
            None,
        )
        .expect("inject later corrupt child");
    repository
        .admit_session_turn(root_session_id, TurnId::new())
        .await
        .expect("root admission")
        .expect("root admitted");
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute(
                "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
                params![root_session_id.to_string(), i64::MAX - 1],
            )
            .expect("order valid blocker before corrupt child");
    }

    let tree_error = repository
        .mutation_blocker_in_session_tree(root_session_id)
        .await
        .expect_err("tree gate must validate the later corrupt child");
    assert!(
        tree_error
            .to_string()
            .contains("incomplete durable run admission")
    );
    let project_error = repository
        .active_session_for_project(project_id)
        .await
        .expect_err("project gate must validate the later corrupt child");
    assert!(
        project_error
            .to_string()
            .contains("incomplete durable run admission")
    );
}

#[tokio::test]
async fn project_gate_includes_unknown_status_rows_without_owner_columns() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let project_id = repository
        .get_session(session_id)
        .await
        .expect("session")
        .project_id;
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .expect("enable corruption fixture");
        connection
            .execute(
                "UPDATE sessions SET status = 'unknown_status' WHERE id = ?1",
                params![session_id.to_string()],
            )
            .expect("inject unknown status");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF")
            .expect("restore constraints");
    }

    let error = repository
        .active_session_for_project(project_id)
        .await
        .expect_err("unknown status must be decoded and rejected");
    assert!(
        error
            .to_string()
            .contains("unknown persisted session status")
    );
}

#[tokio::test]
async fn persisted_access_mode_is_fail_closed_instead_of_defaulting() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .expect("enable corruption fixture");
        connection
            .execute(
                "UPDATE sessions SET access_mode = 'unknown_access' WHERE id = ?1",
                params![session_id.to_string()],
            )
            .expect("inject unknown access mode");
        connection
            .execute_batch("PRAGMA ignore_check_constraints = OFF")
            .expect("restore constraints");
    }

    let error = repository
        .get_session(session_id)
        .await
        .expect_err("unknown persisted access mode must fail closed");
    assert!(error.to_string().contains("unknown persisted access mode"));
}

#[tokio::test]
async fn persisted_auto_review_access_mode_round_trips_through_the_typed_repository() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let initial_revision = repository
        .get_session(session_id)
        .await
        .expect("initial session")
        .session_settings_revision;
    let update = repository
        .compare_and_set_root_session_access_mode(
            session_id,
            AccessMode::Default,
            AccessMode::AutoReview,
        )
        .await
        .expect("auto-review access update")
        .expect("matching access owner");

    assert!(update.changed);
    assert_eq!(update.session.access_mode, AccessMode::AutoReview);
    assert_eq!(
        update.session.session_settings_revision,
        initial_revision + 1
    );
    assert_eq!(
        repository
            .get_session(session_id)
            .await
            .expect("persisted session")
            .access_mode,
        AccessMode::AutoReview
    );
}

#[tokio::test]
async fn provider_connection_round_trips_updates_atomically_and_is_copied_by_forks() {
    let (store, fixture_session_id) = test_repo().await;
    let repository = store.session_repo();
    let fixture = repository
        .get_session(fixture_session_id)
        .await
        .expect("fixture session");
    assert_eq!(
        fixture.provider_connection, None,
        "nullable snapshots preserve the explicit pre-V60 compatibility state"
    );
    let initial_connection = SessionProviderConnection {
        profile: ProviderProfile::OpenAiCompatible,
        api_key_env: Some("OMLX_API_KEY".to_string()),
        extra_headers: BTreeMap::from([("X-Provider".to_string(), "omlx".to_string())]),
    };
    let created = repository
        .create_session(NewSession {
            project_id: fixture.project_id,
            title: "provider snapshot".to_string(),
            cwd: fixture.cwd.clone(),
            model: "openai-compatible-model".to_string(),
            base_url: "http://provider.example:8119/v1".to_string(),
            access_mode: AccessMode::Default,
            provider_connection: Some(initial_connection.clone()),
        })
        .await
        .expect("provider-bound session");
    assert_eq!(
        created.provider_connection.as_ref(),
        Some(&initial_connection)
    );

    let next_connection = SessionProviderConnection {
        profile: ProviderProfile::OpenAiResponses,
        api_key_env: Some("OPENAI_API_KEY".to_string()),
        extra_headers: BTreeMap::new(),
    };
    let update = repository
        .compare_and_set_root_session_settings(
            created.id,
            created.session_settings_revision,
            &SessionSettingsPatch {
                model: Some("gpt-compatible".to_string()),
                base_url: Some("https://provider.example/v1".to_string()),
                provider_connection: Some(next_connection.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("provider connection CAS")
        .expect("matching provider connection revision");
    assert!(update.changed);
    assert_eq!(
        update.session.session_settings_revision,
        created.session_settings_revision + 1
    );
    assert_eq!(
        update.session.provider_connection.as_ref(),
        Some(&next_connection)
    );
    let raw_snapshot = repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT provider_connection_json FROM sessions WHERE id = ?1",
            [created.id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .expect("raw provider snapshot");
    assert_eq!(
        serde_json::from_str::<SessionProviderConnection>(&raw_snapshot)
            .expect("typed raw provider snapshot"),
        next_connection
    );

    let fork = repository
        .fork_session_snapshot(created.id, Some("provider fork".to_string()))
        .await
        .expect("provider-bound fork");
    assert_eq!(
        fork.forked_session.provider_connection,
        update.session.provider_connection
    );

    let paths = store.paths().clone();
    drop(store);
    let reopened_sqlite = SqliteStore::open(&paths).expect("reopen provider store");
    reopened_sqlite
        .migrate()
        .expect("audit reopened provider store");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert_eq!(
        reopened
            .session_repo()
            .get_session(created.id)
            .await
            .expect("reopened provider session")
            .provider_connection,
        Some(next_connection)
    );
}

#[tokio::test]
async fn provider_url_only_settings_updates_clear_hidden_credentials() {
    let (store, fixture_session_id) = test_repo().await;
    let repository = store.session_repo();
    let fixture = repository
        .get_session(fixture_session_id)
        .await
        .expect("fixture session");
    let connection = SessionProviderConnection {
        profile: ProviderProfile::OpenAiCompatible,
        api_key_env: Some("PROVIDER_A_KEY".to_string()),
        extra_headers: BTreeMap::from([(
            "Authorization".to_string(),
            "Bearer provider-a-secret".to_string(),
        )]),
    };
    let created = repository
        .create_session(NewSession {
            project_id: fixture.project_id,
            title: "credential boundary".to_string(),
            cwd: fixture.cwd,
            model: "model-a".to_string(),
            base_url: "https://provider-a.example/v1".to_string(),
            access_mode: AccessMode::Default,
            provider_connection: Some(connection.clone()),
        })
        .await
        .expect("provider-bound session");

    let generic = repository
        .update_session_settings(
            created.id,
            &SessionSettingsPatch {
                base_url: Some("https://provider-b.example/v1".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("generic URL-only update");
    assert_eq!(
        generic.session.provider_connection,
        Some(connection.without_credentials())
    );

    let rebound = repository
        .compare_and_set_root_session_settings(
            created.id,
            generic.session.session_settings_revision,
            &SessionSettingsPatch {
                provider_connection: Some(connection.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("rebind credentials")
        .expect("matching revision");
    let root = repository
        .compare_and_set_root_session_settings(
            created.id,
            rebound.session.session_settings_revision,
            &SessionSettingsPatch {
                base_url: Some("https://provider-c.example/v1".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("root URL-only update")
        .expect("matching revision");
    assert_eq!(
        root.session.provider_connection,
        Some(connection.without_credentials())
    );
}

#[tokio::test]
async fn root_settings_cas_rejects_child_stale_and_active_targets_but_access_stays_live() {
    let (store, root_session_id) = test_repo().await;
    let repository = store.session_repo();
    let initial = repository
        .get_session(root_session_id)
        .await
        .expect("initial root");
    let updated = repository
        .compare_and_set_root_session_settings(
            root_session_id,
            initial.session_settings_revision,
            &SessionSettingsPatch {
                model: Some("updated-model".to_string()),
                context_window: Some(65_536),
                max_output_tokens: Some(4_096),
                ..Default::default()
            },
        )
        .await
        .expect("root settings CAS")
        .expect("matching root revision");
    assert!(updated.changed);
    assert_eq!(updated.session.model, "updated-model");
    assert_eq!(
        updated.session.model_parameters.context_window,
        Some(65_536)
    );
    assert_eq!(
        updated.session.session_settings_revision,
        initial.session_settings_revision + 1
    );

    let stale = repository
        .compare_and_set_root_session_settings(
            root_session_id,
            initial.session_settings_revision,
            &SessionSettingsPatch {
                model: Some("stale-model".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("stale root settings target");
    assert!(stale.is_none());
    let stale_access = repository
        .compare_and_set_root_session_access_mode_at_revision(
            root_session_id,
            initial.session_settings_revision,
            AccessMode::Default,
            AccessMode::FullAccess,
        )
        .await
        .expect("stale access target");
    assert!(stale_access.is_none());

    let child = create_sibling_session(&store, root_session_id, "settings_child").await;
    repository
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/settings_child",
            "settings_child",
        )
        .await
        .expect("child edge");
    let child_error = repository
        .compare_and_set_root_session_settings(
            child.id,
            child.session_settings_revision,
            &SessionSettingsPatch {
                model: Some("forbidden-child-model".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("child session settings owner must fail closed");
    assert!(child_error.to_string().contains("child agent session"));
    let child_access_error = repository
        .compare_and_set_root_session_access_mode_at_revision(
            child.id,
            child.session_settings_revision,
            AccessMode::Default,
            AccessMode::FullAccess,
        )
        .await
        .expect_err("child access owner must fail closed");
    assert!(
        child_access_error
            .to_string()
            .contains("child agent session")
    );

    repository
        .admit_session_turn(root_session_id, TurnId::new())
        .await
        .expect("root admission")
        .expect("root admitted");
    let active_error = repository
        .compare_and_set_root_session_settings(
            root_session_id,
            updated.session.session_settings_revision,
            &SessionSettingsPatch {
                model: Some("active-model".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("generic settings must reject an active root");
    assert!(matches!(
        active_error,
        StorageError::SessionSettingsActiveTree {
            root_session_id: blocked_root,
            active_session_id,
        } if blocked_root == root_session_id && active_session_id == root_session_id
    ));

    let access = repository
        .compare_and_set_root_session_access_mode_at_revision(
            root_session_id,
            updated.session.session_settings_revision,
            AccessMode::Default,
            AccessMode::FullAccess,
        )
        .await
        .expect("access decision-time CAS while active")
        .expect("matching access mode");
    assert!(access.changed);
    assert_eq!(access.session.access_mode, AccessMode::FullAccess);
    assert_eq!(
        access.session.session_settings_revision,
        updated.session.session_settings_revision + 1
    );
}

#[tokio::test]
async fn concurrent_root_settings_writers_commit_exactly_one_revision() {
    let (store, session_id) = test_repo().await;
    let second_store = StoreBundle::new(
        SqliteStore::open(store.paths()).expect("second process-like SQLite connection"),
    );
    let expected_revision = store
        .session_repo()
        .get_session(session_id)
        .await
        .expect("initial session")
        .session_settings_revision;
    let barrier = Arc::new(std::sync::Barrier::new(3));

    let spawn_writer = |repository: SqliteSessionRepository,
                        barrier: Arc<std::sync::Barrier>,
                        model: &'static str| {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("writer runtime");
            barrier.wait();
            runtime.block_on(repository.compare_and_set_root_session_settings(
                session_id,
                expected_revision,
                &SessionSettingsPatch {
                    model: Some(model.to_string()),
                    ..Default::default()
                },
            ))
        })
    };
    let first = spawn_writer(
        store.session_repo(),
        Arc::clone(&barrier),
        "concurrent-model-a",
    );
    let second = spawn_writer(
        second_store.session_repo(),
        Arc::clone(&barrier),
        "concurrent-model-b",
    );
    barrier.wait();

    let outcomes = [
        first
            .join()
            .expect("first writer thread")
            .expect("first CAS"),
        second
            .join()
            .expect("second writer thread")
            .expect("second CAS"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.as_ref().is_some_and(|update| update.changed))
            .count(),
        1
    );
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_none()).count(),
        1
    );
    let persisted = store
        .session_repo()
        .get_session(session_id)
        .await
        .expect("persisted winner");
    assert_eq!(persisted.session_settings_revision, expected_revision + 1);
    assert!(matches!(
        persisted.model.as_str(),
        "concurrent-model-a" | "concurrent-model-b"
    ));
}

async fn active_turn(store: &StoreBundle, session_id: SessionId) -> (AdmissionId, TurnId) {
    let repo = store.session_repo();
    let turn_id = TurnId::new();
    let admission_id = repo
        .admit_session_turn(session_id, turn_id)
        .await
        .expect("admit")
        .expect("admitted")
        .admission_id;
    repo.append_user_turn_with_protocol_bundle(
        session_id,
        admission_id,
        &UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: "canonical request".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        },
        turn_id,
        0,
    )
    .await
    .expect("user turn");
    (admission_id, turn_id)
}

fn started_harness_run(
    run_id: HarnessRunId,
    session_id: SessionId,
    turn_id: TurnId,
) -> HarnessRunRecord {
    HarnessRunRecord {
        id: run_id,
        session_id: Some(session_id),
        protocol_turn_id: Some(turn_id),
        canonical_terminal_runtime_event_id: None,
        workspace_root: "C:/workspace".into(),
        artifact_root: Utf8PathBuf::from(format!("C:/artifacts/{run_id}")),
        mode: "native_runtime".to_string(),
        started_at_ms: 10,
        completed_at_ms: None,
        status: HarnessRunStatus::Started,
    }
}

#[tokio::test]
async fn delivered_steer_advances_the_durable_permission_fence_generation() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let first_authority = store
        .protocol_event_store()
        .canonical_user_authority_items_for_session(session_id)
        .expect("canonical UserTurn authority")
        .last()
        .expect("initial authority")
        .id;
    let key = crate::storage::PermissionRetryFenceKey::new(
        session_id,
        1,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("retry key");
    let first_lease = match store
        .permission_retry_fence_store()
        .begin_review(
            key.clone(),
            first_authority,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .expect("claim initial authority")
    {
        crate::storage::BeginPermissionReview::Claimed(lease) => lease,
        other => panic!("expected initial claim, got {other:?}"),
    };
    assert_eq!(
        first_lease
            .mark_denied(crate::storage::PermissionRetryFenceOutcome::GuardianDenied)
            .expect("persist denial"),
        crate::storage::PermissionReviewTransition::Applied
    );
    drop(first_lease);

    store
        .session_repo()
        .accept_active_turn_steer(
            session_id,
            1,
            &SteerTurn {
                expected_turn_id: turn_id,
                items: vec![UserInputItem::Text {
                    text: "authorize a new elevated review".to_string(),
                }],
                additional_context: Default::default(),
                client_user_message_id: Some("permission-unlock-steer".to_string()),
            },
        )
        .await
        .expect("queue canonical SteerTurn");
    assert_eq!(
        store
            .session_repo()
            .deliver_all_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
            .expect("deliver canonical SteerTurn")
            .len(),
        1
    );
    let second_authority = store
        .protocol_event_store()
        .canonical_user_authority_items_for_session(session_id)
        .expect("canonical authority after steer")
        .last()
        .expect("steer authority")
        .id;
    assert_ne!(first_authority, second_authority);
    let second_lease = match store
        .permission_retry_fence_store()
        .begin_review(
            key,
            second_authority,
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        )
        .expect("claim new authority generation")
    {
        crate::storage::BeginPermissionReview::Claimed(lease) => lease,
        other => panic!("a delivered steer must unlock one new review, got {other:?}"),
    };
    second_lease.release().expect("release steer fixture");
}

#[tokio::test]
async fn failed_steer_transaction_is_invisible_and_retry_commits_once() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let repository = store.session_repo();
    repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch(
            "CREATE TRIGGER abort_steer_queue
                 BEFORE INSERT ON turn_steer_inputs
                 WHEN NEW.origin_kind = 'runtime'
                 BEGIN SELECT RAISE(ABORT, 'injected steer queue failure'); END;",
        )
        .expect("failure trigger");
    let steer = SteerTurn {
        expected_turn_id: turn_id,
        items: vec![UserInputItem::Text {
            text: "retry this steer".to_string(),
        }],
        additional_context: Default::default(),
        client_user_message_id: Some("retry-steer".to_string()),
    };

    repository
        .accept_active_turn_steer(session_id, 1, &steer)
        .await
        .expect_err("injected transaction failure");
    assert_eq!(
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*)
                     FROM turn_steer_inputs
                     WHERE session_id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("queue count after failure"),
        0
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history after failure")
            .iter()
            .all(|item| !matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
    );
    assert!(
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("events after failure")
            .iter()
            .all(|event| !matches!(event.msg, RuntimeEventMsg::SteerInputAccepted { .. }))
    );
    assert!(
        store
            .protocol_event_store()
            .list_turn_items(session_id, turn_id)
            .expect("turn items after failure")
            .iter()
            .all(|item| !matches!(item.payload, TurnItemPayload::SteerMessage { .. }))
    );

    repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch("DROP TRIGGER abort_steer_queue;")
        .expect("drop failure trigger");
    let committed_id = repository
        .accept_active_turn_steer(session_id, 1, &steer)
        .await
        .expect("retry steer");
    assert!(
        repository
            .has_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
            .expect("pending retry steer")
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history before safe delivery")
            .iter()
            .all(|item| !matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
    );
    assert_eq!(
        repository
            .deliver_all_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
            .expect("safe steer delivery"),
        vec![committed_id]
    );
    let committed = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history after retry")
        .into_iter()
        .filter(|item| matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
        .collect::<Vec<_>>();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].id, committed_id);
    assert!(
        !repository
            .has_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
            .expect("no pending steer after delivery")
    );
}

#[tokio::test]
async fn queued_steers_deliver_in_fifo_order_with_exact_history_identities() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let repository = store.session_repo();
    let mut accepted_ids = Vec::new();
    for index in 0..3 {
        accepted_ids.push(
            repository
                .accept_active_turn_steer(
                    session_id,
                    1,
                    &SteerTurn {
                        expected_turn_id: turn_id,
                        items: vec![UserInputItem::Text {
                            text: format!("fifo-steer-{index}"),
                        }],
                        additional_context: Default::default(),
                        client_user_message_id: Some(format!("fifo-client-{index}")),
                    },
                )
                .await
                .expect("queue FIFO steer"),
        );
    }
    assert!(
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history before FIFO delivery")
            .iter()
            .all(|item| !matches!(item.payload, HistoryItemPayload::SteerTurn { .. }))
    );

    assert_eq!(
        repository
            .deliver_all_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
            .expect("deliver FIFO batch"),
        accepted_ids
    );
    let delivered_ids = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history after FIFO delivery")
        .into_iter()
        .filter_map(|item| {
            matches!(item.payload, HistoryItemPayload::SteerTurn { .. }).then_some(item.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(delivered_ids, accepted_ids);
    assert_eq!(
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("runtime events after FIFO delivery")
            .into_iter()
            .filter(|event| { matches!(event.msg, RuntimeEventMsg::SteerInputAccepted { .. }) })
            .count(),
        accepted_ids.len()
    );
}

#[tokio::test]
async fn failed_delivery_rolls_back_the_whole_batch_and_retry_keeps_the_same_id() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let repository = store.session_repo();
    let input_id = repository
        .accept_active_turn_steer(
            session_id,
            1,
            &SteerTurn {
                expected_turn_id: turn_id,
                items: vec![UserInputItem::Text {
                    text: "atomic delivery".to_string(),
                }],
                additional_context: Default::default(),
                client_user_message_id: Some("atomic-delivery".to_string()),
            },
        )
        .await
        .expect("queue steer");
    repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch(
            "CREATE TRIGGER abort_steer_turn_projection
                 BEFORE INSERT ON protocol_turn_items
                 WHEN json_extract(NEW.payload_json, '$.kind') = 'steer_message'
                 BEGIN SELECT RAISE(ABORT, 'injected steer projection failure'); END;",
        )
        .expect("install delivery failure");

    let error = repository
        .deliver_all_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id)
        .expect_err("delivery bundle must roll back");
    assert!(
        error
            .to_string()
            .contains("injected steer projection failure")
    );
    assert!(
        repository
            .has_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
            .expect("queue retained after rollback")
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history after rollback")
            .iter()
            .all(|item| item.id != input_id)
    );
    assert!(
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("runtime events after rollback")
            .iter()
            .all(|event| !matches!(event.msg, RuntimeEventMsg::SteerInputAccepted { .. }))
    );

    repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch("DROP TRIGGER abort_steer_turn_projection")
        .expect("remove delivery failure");
    assert_eq!(
        repository
            .deliver_all_pending_turn_steers_for_admitted_turn(session_id, admission_id, turn_id,)
            .expect("retry delivery"),
        vec![input_id]
    );
}

#[tokio::test]
async fn normal_terminal_finish_drains_late_steer_but_interruption_discards_it() {
    let (completed_store, completed_session_id) = test_repo().await;
    let (completed_admission_id, completed_turn_id) =
        active_turn(&completed_store, completed_session_id).await;
    let completed_repo = completed_store.session_repo();
    let completed_input_id = completed_repo
        .accept_active_turn_steer(
            completed_session_id,
            1,
            &SteerTurn {
                expected_turn_id: completed_turn_id,
                items: vec![UserInputItem::Text {
                    text: "late normal steer".to_string(),
                }],
                additional_context: Default::default(),
                client_user_message_id: Some("late-normal".to_string()),
            },
        )
        .await
        .expect("queue late normal steer");
    assert_eq!(
        completed_repo
            .terminalize_admitted_turn_with_protocol_event(
                completed_session_id,
                completed_admission_id,
                &completed_terminal_for_response(completed_session_id, None),
                completed_turn_id,
                None,
                None,
            )
            .await
            .expect("normal terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        completed_store
            .protocol_event_store()
            .list_history_items(completed_session_id, completed_turn_id)
            .expect("normal terminal history")
            .iter()
            .any(|item| item.id == completed_input_id)
    );
    let completed_state = completed_repo
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT state, delivered_history_item_id,
                        resolved_by_terminal_event_id
                 FROM turn_steer_inputs
                 WHERE id = ?1",
            [completed_input_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .expect("normal terminal queue state");
    assert_eq!(
        completed_state,
        (
            "delivered".to_string(),
            Some(completed_input_id.to_string()),
            None
        )
    );

    let (interrupted_store, interrupted_session_id) = test_repo().await;
    let (interrupted_admission_id, interrupted_turn_id) =
        active_turn(&interrupted_store, interrupted_session_id).await;
    let interrupted_repo = interrupted_store.session_repo();
    let interrupted_input_id = interrupted_repo
        .accept_active_turn_steer(
            interrupted_session_id,
            1,
            &SteerTurn {
                expected_turn_id: interrupted_turn_id,
                items: vec![UserInputItem::Text {
                    text: "discard on interrupt".to_string(),
                }],
                additional_context: Default::default(),
                client_user_message_id: Some("interrupt-discard".to_string()),
            },
        )
        .await
        .expect("queue interrupt steer");
    assert_eq!(
        interrupted_repo
            .terminalize_admitted_turn_with_protocol_event(
                interrupted_session_id,
                interrupted_admission_id,
                &agent_interrupted_terminal(interrupted_session_id),
                interrupted_turn_id,
                None,
                None,
            )
            .await
            .expect("interrupt terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        interrupted_store
            .protocol_event_store()
            .list_history_items(interrupted_session_id, interrupted_turn_id)
            .expect("interrupt history")
            .iter()
            .all(|item| item.id != interrupted_input_id)
    );
    let (state, resolver_id) = interrupted_repo
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT state, resolved_by_terminal_event_id
                 FROM turn_steer_inputs
                 WHERE id = ?1",
            [interrupted_input_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("interrupt queue state");
    assert_eq!(state, "discarded");
    let resolver_id = resolver_id
        .parse::<RuntimeEventId>()
        .expect("typed interrupt terminal id");
    let resolver = interrupted_store
        .protocol_event_store()
        .list_runtime_events(interrupted_session_id, interrupted_turn_id)
        .expect("interrupt resolver events")
        .into_iter()
        .filter(|event| event.id == resolver_id)
        .collect::<Vec<_>>();
    assert!(matches!(
        resolver.as_slice(),
        [RuntimeEvent {
            msg: RuntimeEventMsg::TurnTerminal { terminal },
            ..
        }] if matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. })
    ));
}

async fn expire_and_recover_run(store: &StoreBundle, session_id: SessionId) -> AdmissionId {
    let recovery_now = SystemClock::now_ms()
        .saturating_add(RUN_ADMISSION_LEASE_DURATION_MS)
        .saturating_add(1_000);
    store
        .session_repo()
        .admit_session_turn_at(
            session_id,
            TurnId::new(),
            recovery_now,
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
        .expect("recover expired admission")
        .expect("admit replacement run")
        .admission_id
}

fn completed_terminal(session_id: SessionId) -> RunEvent {
    RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(crate::session::model::DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Completed,
            final_response_id: Some(ModelResponseId::new()),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    }
}

fn completed_terminal_for_response(
    session_id: SessionId,
    final_response_id: Option<ModelResponseId>,
) -> RunEvent {
    RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(crate::session::model::DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Completed,
            final_response_id,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    }
}

async fn record_text_response(
    store: &StoreBundle,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    text: &str,
) -> ModelResponseId {
    let response_id = ModelResponseId::new();
    store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id,
                assistant_text: Some(text.to_string()),
                assistant_protocol_sequence_no: None,
                tool_calls: Vec::new(),
            },
        )
        .await
        .expect("record assistant response");
    response_id
}

async fn completed_turn_with_retained_admission(
    store: &StoreBundle,
    session_id: SessionId,
) -> (AdmissionId, TurnId) {
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(store, session_id).await;
    let target = repository
        .captured_running_terminal_target(session_id)
        .await
        .expect("capture terminal target")
        .expect("running terminal target");
    assert!(
        repository
            .terminalize_captured_running_session_with_protocol_event(
                session_id,
                &completed_terminal(session_id),
                target,
            )
            .await
            .expect("terminalize while retaining admission")
    );
    (admission_id, turn_id)
}

fn delete_terminal_runtime_event_for_corruption_test(
    store: &StoreBundle,
    session_id: SessionId,
    turn_id: TurnId,
) {
    store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute(
            "DELETE FROM protocol_runtime_events
                 WHERE session_id = ?1
                   AND turn_id = ?2
                   AND json_extract(msg_json, '$.kind') = 'turn_terminal'",
            params![session_id.to_string(), turn_id.to_string()],
        )
        .expect("delete terminal corruption fixture");
}

fn inject_duplicate_terminal_runtime_event_for_corruption_test(
    store: &StoreBundle,
    session_id: SessionId,
    turn_id: TurnId,
) {
    let duplicate = project_protocol_run_event(
        &failed_terminal(session_id, "duplicate terminal"),
        Some(session_id),
        turn_id,
        100,
    )
    .expect("duplicate projection")
    .runtime_event;
    let duplicate_json = serde_json::to_string(&duplicate.msg).expect("duplicate JSON");
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    connection
        .execute_batch("DROP INDEX idx_protocol_runtime_events_unique_turn_terminal")
        .expect("remove unique terminal index for corruption fixture");
    connection
        .execute(
            "INSERT INTO protocol_runtime_events
                 (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'corrupt-fixture', ?6)",
            params![
                duplicate.id.to_string(),
                session_id.to_string(),
                turn_id.to_string(),
                duplicate.sequence_no,
                duplicate_json,
                duplicate.created_at_ms,
            ],
        )
        .expect("inject duplicate terminal");
}

fn failed_terminal(session_id: SessionId, error: &str) -> RunEvent {
    RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(crate::session::model::DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Failed {
                error: error.to_string(),
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    }
}

/// Builds the historical V48 state explicitly. Current runtime completion never creates
/// `completed_early`; this fixture exists only to keep backward-compatible reads and cleanup
/// of already-persisted rows covered.
fn replace_current_handoff_with_legacy_completed_early(
    store: &StoreBundle,
    agent_session_id: SessionId,
    agent_turn_id: TurnId,
    parent_session_id: SessionId,
) {
    let repository = store.session_repo();
    let mut connection = repository.connection.lock().expect("sqlite mutex");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("legacy completed-early fixture transaction");
    let mailbox_id = transaction
        .query_row(
            "SELECT parent_history_item_id
                 FROM agent_completion_handoffs
                 WHERE child_session_id = ?1 AND child_turn_id = ?2",
            params![agent_session_id.to_string(), agent_turn_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("current completion handoff query")
        .expect("current completion handoff");
    transaction
        .execute(
            "DELETE FROM agent_completion_handoffs
                 WHERE child_session_id = ?1 AND child_turn_id = ?2",
            params![agent_session_id.to_string(), agent_turn_id.to_string()],
        )
        .expect("remove current completion handoff");
    transaction
        .execute(
            "DELETE FROM protocol_item_append_order
                 WHERE session_id = ?1
                   AND source_kind = 'mailbox_message'
                   AND source_id = ?2",
            params![parent_session_id.to_string(), mailbox_id],
        )
        .expect("remove current mailbox append order");
    transaction
        .execute(
            "DELETE FROM agent_mailbox_messages WHERE id = ?1",
            params![mailbox_id],
        )
        .expect("remove current mailbox");
    insert_deferred_agent_completion_in_transaction(
        &transaction,
        agent_session_id,
        agent_turn_id,
        parent_session_id,
        DeferredAgentCompletionKind::CompletedEarly,
        normalize_run_lease_now_ms(SystemClock::now_ms()),
    )
    .expect("insert historical completed-early receipt");
    transaction
        .commit()
        .expect("commit historical completed-early fixture");
}

fn stored_admission_state(
    store: &StoreBundle,
    session_id: SessionId,
) -> (String, Option<String>, Option<String>, Option<i64>) {
    store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT status, active_run_id, active_turn_id, active_run_lease_expires_at_ms
                 FROM sessions WHERE id = ?1",
            params![session_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .expect("stored admission state")
}

fn protocol_turn_table_counts(
    store: &StoreBundle,
    session_id: SessionId,
    turn_id: TurnId,
) -> Vec<(&'static str, i64)> {
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    [
        "protocol_runtime_events",
        "protocol_history_items",
        "protocol_turn_items",
        "protocol_item_append_order",
        "protocol_turn_sequence_allocators",
    ]
    .into_iter()
    .map(|table| {
        let count = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1 AND turn_id = ?2"),
                params![session_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("protocol table count");
        (table, count)
    })
    .collect()
}

fn text_user_turn(turn_id: TurnId, text: &str) -> UserTurn {
    UserTurn {
        turn_id,
        items: vec![UserInputItem::Text {
            text: text.to_string(),
        }],
        prompt_dispatch: None,
        editor_context: None,
    }
}

#[tokio::test]
async fn child_terminal_and_immediate_parent_final_commit_once_with_exact_response() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let exact_response_id = record_text_response(
        &store,
        child.id,
        child_admission_id,
        child_turn_id,
        "  exact child result  ",
    )
    .await;
    record_text_response(
        &store,
        child.id,
        child_admission_id,
        child_turn_id,
        "unrelated later assistant",
    )
    .await;

    let terminal = completed_terminal_for_response(child.id, Some(exact_response_id));
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &terminal,
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("atomic child terminal"),
        AdmittedTerminalCommit::Applied
    );

    let handoff = store
        .session_repo()
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("handoff query")
        .expect("handoff receipt");
    assert_eq!(handoff.parent_session_id, root_session_id);
    assert_eq!(handoff.parent_agent_path.as_str(), "/root");
    assert!(
        store
            .protocol_event_store()
            .list_history_items(root_session_id, root_turn_id)
            .expect("parent history before safe delivery")
            .into_iter()
            .all(|item| item.id != handoff.history_item_id)
    );
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                root_admission_id,
                &completed_terminal_for_response(root_session_id, None),
                root_turn_id,
                None,
                None,
            )
            .await
            .expect("parent terminal with finish-drain"),
        AdmittedTerminalCommit::Applied
    );
    let final_items = store
        .protocol_event_store()
        .list_history_items(root_session_id, root_turn_id)
        .expect("parent turn history")
        .into_iter()
        .filter(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == "/root/child"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(final_items.len(), 1);
    assert_eq!(final_items[0].id, handoff.history_item_id);
    let HistoryItemPayload::InterAgentCommunication { communication } = &final_items[0].payload
    else {
        unreachable!("filtered parent FINAL");
    };
    assert_eq!(
        communication.content,
        "Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/child\nPayload:\n  exact child result  "
    );
    assert!(!communication.content.contains("unrelated later assistant"));

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &terminal,
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("duplicate child terminal attempt"),
        AdmittedTerminalCommit::NotOwned
    );
    let session_repo = store.session_repo();
    let connection = session_repo.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM agent_completion_handoffs",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .expect("receipt count"),
        1
    );
    drop(connection);

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                root_admission_id,
                &completed_terminal_for_response(root_session_id, None),
                root_turn_id,
                None,
                None,
            )
            .await
            .expect("parent terminal after durable child FINAL"),
        AdmittedTerminalCommit::NotOwned
    );
    let rollback_error = store
        .session_repo()
        .rollback_session_transaction(child.id, 1)
        .await
        .expect_err("handoff participant rollback must be rejected");
    assert!(
        rollback_error
            .to_string()
            .contains("durable agent completion handoff")
    );
    let deleted = store
        .session_repo()
        .delete_session_tree(root_session_id)
        .await
        .expect("leaf-first agent tree delete");
    assert_eq!(deleted.len(), 2);
    assert_eq!(
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT COUNT(*) FROM agent_completion_handoffs",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .expect("receipt count after tree delete"),
        0
    );
}

#[tokio::test]
async fn oversized_completed_child_handoff_preserves_the_exact_result_and_receipt_identity() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let oversized = format!(
        "completed-result-head:{}:completed-result-tail",
        "0123456789".repeat(1_200)
    );
    let response_id = record_text_response(
        &store,
        child.id,
        child_admission_id,
        child_turn_id,
        &oversized,
    )
    .await;

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, Some(response_id)),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("oversized child terminal"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("handoff query")
        .expect("handoff receipt");
    assert_eq!(handoff.child_session_id, child.id);
    assert_eq!(handoff.child_turn_id, child_turn_id);
    assert_eq!(handoff.parent_session_id, root_session_id);
    assert_eq!(handoff.parent_agent_path.as_str(), "/root");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                root_session_id,
                root_admission_id,
                root_turn_id,
                128,
            )
            .expect("safe oversized completion delivery")
            .history_item_ids,
        vec![handoff.history_item_id]
    );

    let items = store
        .protocol_event_store()
        .history_items_by_id(root_session_id, &[handoff.history_item_id])
        .expect("exact receipt history");
    let [item] = items.as_slice() else {
        panic!("completion receipt must reference one exact parent history item");
    };
    assert_eq!(item.id, handoff.history_item_id);
    let HistoryItemPayload::InterAgentCommunication { communication } = &item.payload else {
        panic!("completion receipt must reference inter-agent communication");
    };
    assert_eq!(communication.author, "/root/child");
    assert_eq!(communication.recipient, "/root");
    let (_, payload) = communication
        .content
        .split_once("Payload:\n")
        .expect("completion envelope");
    assert_eq!(payload, oversized);
}

#[tokio::test]
async fn oversized_failed_child_error_is_middle_truncated_for_the_exact_immediate_parent() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            child.id,
            grandchild.id,
            "/root/child/grandchild",
            "grandchild",
        )
        .await
        .expect("grandchild edge");
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let (grandchild_admission_id, grandchild_turn_id) = active_turn(&store, grandchild.id).await;
    let oversized_error = format!(
        "failed-error-head:{}:failed-error-tail",
        "abcdefghij".repeat(1_200)
    );

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                grandchild.id,
                grandchild_admission_id,
                &failed_terminal(grandchild.id, &oversized_error),
                grandchild_turn_id,
                None,
                None,
            )
            .await
            .expect("oversized grandchild failure terminal"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(grandchild.id, grandchild_turn_id)
        .expect("handoff query")
        .expect("handoff receipt");
    assert_eq!(handoff.child_session_id, grandchild.id);
    assert_eq!(handoff.child_turn_id, grandchild_turn_id);
    assert_eq!(handoff.parent_session_id, child.id);
    assert_eq!(handoff.parent_agent_path.as_str(), "/root/child");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                child.id,
                child_admission_id,
                child_turn_id,
                128,
            )
            .expect("safe oversized failure delivery")
            .history_item_ids,
        vec![handoff.history_item_id]
    );

    let items = store
        .protocol_event_store()
        .history_items_by_id(child.id, &[handoff.history_item_id])
        .expect("exact receipt history");
    let [item] = items.as_slice() else {
        panic!("failure receipt must reference one exact parent history item");
    };
    assert_eq!(item.id, handoff.history_item_id);
    let HistoryItemPayload::InterAgentCommunication { communication } = &item.payload else {
        panic!("failure receipt must reference inter-agent communication");
    };
    assert_eq!(communication.author, "/root/child/grandchild");
    assert_eq!(communication.recipient, "/root/child");
    let (_, payload) = communication
        .content
        .split_once("Payload:\n")
        .expect("completion envelope");
    let error = payload
        .strip_prefix("Agent errored: ")
        .and_then(|payload| {
            payload.strip_suffix(&format!("\n\n{AGENT_COMPLETION_ERROR_NEXT_ACTION}"))
        })
        .expect("bounded Codex error payload");
    assert_ne!(error, oversized_error);
    assert!(error.starts_with("failed-error-head:"));
    assert!(error.ends_with(":failed-error-tail"));
    assert!(error.contains(" tokens truncated…"));
    assert!(
        crate::context::context_window::estimate_text_tokens(error)
            <= AGENT_COMPLETION_ERROR_MAX_TOKENS
    );
    assert!(
        crate::context::context_window::estimate_text_tokens(payload)
            <= AGENT_COMPLETION_MESSAGE_MAX_TOKENS
    );
}

#[tokio::test]
async fn completion_handoff_failure_rolls_back_terminal_and_parent_final_together() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    let (_, root_turn_id) = active_turn(&store, root_session_id).await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let response_id = record_text_response(
        &store,
        child.id,
        child_admission_id,
        child_turn_id,
        "must commit atomically",
    )
    .await;
    store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch(
            "CREATE TRIGGER abort_agent_completion_receipt
                 BEFORE INSERT ON agent_completion_handoffs
                 BEGIN
                     SELECT RAISE(ABORT, 'injected completion receipt failure');
                 END;",
        )
        .expect("failure trigger");

    let error = store
        .session_repo()
        .terminalize_admitted_turn_with_protocol_event(
            child.id,
            child_admission_id,
            &completed_terminal_for_response(child.id, Some(response_id)),
            child_turn_id,
            None,
            None,
        )
        .await
        .expect_err("receipt failure must abort whole terminal transaction");
    assert!(
        error
            .to_string()
            .contains("injected completion receipt failure")
    );
    assert_eq!(stored_admission_state(&store, child.id).0, "running");
    assert!(
        terminal_for_turn_in_connection(
            &store
                .session_repo()
                .connection
                .lock()
                .expect("sqlite mutex"),
            child.id,
            child_turn_id,
        )
        .expect("terminal lookup after rollback")
        .is_none()
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(root_session_id, root_turn_id)
            .expect("parent history after rollback")
            .into_iter()
            .all(|item| !matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
    );
    store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch("DROP TRIGGER abort_agent_completion_receipt;")
        .expect("drop failure trigger");

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, Some(response_id)),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("retry terminal transaction"),
        AdmittedTerminalCommit::Applied
    );
}

#[tokio::test]
async fn nested_failure_handoff_targets_stale_immediate_parent_session_scope() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    let grandchild = create_sibling_session(&store, root_session_id, "grandchild").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            child.id,
            grandchild.id,
            "/root/child/grandchild",
            "grandchild",
        )
        .await
        .expect("grandchild edge");
    store
        .session_repo()
        .admit_session_turn_at(child.id, TurnId::new(), 1, 1)
        .await
        .expect("stale parent admission")
        .expect("stale parent admitted");
    let (grandchild_admission_id, grandchild_turn_id) = active_turn(&store, grandchild.id).await;

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                grandchild.id,
                grandchild_admission_id,
                &failed_terminal(grandchild.id, "bounded failure"),
                grandchild_turn_id,
                None,
                None,
            )
            .await
            .expect("grandchild failure terminal"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(grandchild.id, grandchild_turn_id)
        .expect("nested handoff query")
        .expect("nested handoff");
    assert_eq!(handoff.parent_session_id, child.id);
    assert_eq!(handoff.parent_agent_path.as_str(), "/root/child");
    assert_eq!(
        store
            .session_repo()
            .schedulable_owner_resume_request_id(child.id)
            .expect("expired parent continuation"),
        None,
        "an expired running parent is recovered by the runtime owner before an OwnerResume can be scheduled"
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(child.id)
            .expect("child canonical history before safe delivery")
            .into_iter()
            .all(|item| item.id != handoff.history_item_id)
    );
    let child_mail = store
        .session_repo()
        .agent_mailbox_communications_by_id(child.id, &[handoff.history_item_id])
        .expect("queued nested failure");
    assert!(matches!(
        child_mail.as_slice(),
        [(id, communication)]
            if *id == handoff.history_item_id
                && communication.content
                    == "Message Type: FINAL_ANSWER\nTask name: /root/child\nSender: /root/child/grandchild\nPayload:\nAgent errored: bounded failure\n\nThis agent's turn failed. If you still need this agent, use the available collaboration tools to give it another task."
    ));
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("root canonical history")
            .into_iter()
            .all(|item| !matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
    );
}

#[tokio::test]
async fn completed_child_without_a_response_delivers_an_empty_final_payload() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    let (admission_id, turn_id) = active_turn(&store, child.id).await;

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                admission_id,
                &completed_terminal_for_response(child.id, None),
                turn_id,
                None,
                None,
            )
            .await
            .expect("empty child completion"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(child.id, turn_id)
        .expect("handoff query")
        .expect("empty completion handoff");
    let root_turn_id = TurnId::new();
    let root_admission = store
        .session_repo()
        .admit_session_turn(root_session_id, root_turn_id)
        .await
        .expect("root safe-delivery admission")
        .expect("root admitted");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(
                root_session_id,
                root_admission.admission_id,
                root_turn_id,
                128,
            )
            .expect("safe empty completion delivery")
            .history_item_ids,
        vec![handoff.history_item_id]
    );
    let final_content = store
        .protocol_event_store()
        .list_history_items_for_session(root_session_id)
        .expect("root history")
        .into_iter()
        .find_map(|item| match item.payload {
            HistoryItemPayload::InterAgentCommunication { communication }
                if item.id == handoff.history_item_id =>
            {
                Some(communication.content)
            }
            _ => None,
        })
        .expect("empty completion FINAL");
    assert_eq!(
        final_content,
        "Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/child\nPayload:\n"
    );
}

#[tokio::test]
async fn completed_child_does_not_revive_a_cancelled_immediate_parent() {
    let (store, root_session_id) = test_repo().await;
    let child = create_sibling_session(&store, root_session_id, "child").await;
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("child edge");
    let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let interrupted_root = RunEvent::TurnTerminal {
        session_id: root_session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                root_session_id,
                root_admission_id,
                &interrupted_root,
                root_turn_id,
                None,
                None,
            )
            .await
            .expect("cancel parent"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, None),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("complete child after parent cancellation"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &tree_stopped_terminal(child.id),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("late tree-stop terminal"),
        AdmittedTerminalCommit::NotOwned
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("handoff query")
        .expect("completed child FINAL");
    let parent_finals = store
        .session_repo()
        .agent_mailbox_communications_by_id(root_session_id, &[handoff.history_item_id])
        .expect("queued parent FINAL");
    assert!(matches!(
        parent_finals.as_slice(),
        [(_, communication)] if !communication.trigger_turn
    ));
    assert!(
        store
            .session_repo()
            .schedulable_owner_resume_request_id(root_session_id)
            .expect("cancelled parent owner resume")
            .is_none()
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("root history")
            .into_iter()
            .all(|item| !matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
    );
}

#[tokio::test]
async fn new_and_resumed_turns_admit_run_and_turn_as_one_owner() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let first_turn_id = TurnId::new();
    let first_admission_id = repository
        .admit_session_turn(session_id, first_turn_id)
        .await
        .expect("first admission")
        .expect("first turn admitted")
        .admission_id;

    let first_state = stored_admission_state(&store, session_id);
    assert_eq!(first_state.0, "running");
    assert_eq!(first_state.1, Some(first_admission_id.to_string()));
    assert_eq!(first_state.2, Some(first_turn_id.to_string()));
    assert!(first_state.3.is_some());

    let terminal = completed_terminal(session_id);
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                first_admission_id,
                &terminal,
                first_turn_id,
                None,
                None,
            )
            .await
            .expect("terminal commit"),
        AdmittedTerminalCommit::Applied
    );

    let resumed_turn_id = TurnId::new();
    let resumed_admission_id = repository
        .admit_session_turn(session_id, resumed_turn_id)
        .await
        .expect("resumed admission")
        .expect("resumed turn admitted")
        .admission_id;
    let resumed_state = stored_admission_state(&store, session_id);
    assert_eq!(resumed_state.0, "running");
    assert_eq!(resumed_state.1, Some(resumed_admission_id.to_string()));
    assert_eq!(resumed_state.2, Some(resumed_turn_id.to_string()));
    assert!(resumed_state.3.is_some());
}

#[tokio::test]
async fn admission_rejects_every_prior_turn_identity_trace() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, used_turn_id) = active_turn(&store, session_id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &completed_terminal(session_id),
                used_turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize used turn"),
        AdmittedTerminalCommit::Applied
    );

    let reused_error = repository
        .admit_session_turn(session_id, used_turn_id)
        .await
        .expect_err("canonical turn identity must never be reusable");
    assert!(reused_error.to_string().contains("has already been used"));

    let allocator_only_turn_id = TurnId::new();
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute(
                "INSERT INTO protocol_turn_sequence_allocators
                     (session_id, turn_id, next_sequence_no)
                     VALUES (?1, ?2, 0)",
                params![session_id.to_string(), allocator_only_turn_id.to_string()],
            )
            .expect("inject orphan allocator trace");
    }
    let allocator_error = repository
        .admit_session_turn(session_id, allocator_only_turn_id)
        .await
        .expect_err("allocator trace must fence turn identity reuse");
    assert!(
        allocator_error
            .to_string()
            .contains("has already been used")
    );
    assert!(
        !repository
            .has_fresh_run_admission(session_id)
            .await
            .expect("no admission after collisions")
    );
}

#[tokio::test]
async fn terminal_lease_outcome_is_exact_to_the_requested_nonreusable_turn() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (first_admission_id, first_turn_id) = active_turn(&store, session_id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                first_admission_id,
                &completed_terminal(session_id),
                first_turn_id,
                None,
                None,
            )
            .await
            .expect("complete first turn"),
        AdmittedTerminalCommit::Applied
    );
    let (second_admission_id, second_turn_id) = active_turn(&store, session_id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                second_admission_id,
                &failed_terminal(session_id, "second failed"),
                second_turn_id,
                None,
                None,
            )
            .await
            .expect("fail second turn"),
        AdmittedTerminalCommit::Applied
    );

    assert!(matches!(
        repository
            .renew_admitted_run_lease(session_id, first_admission_id, first_turn_id)
            .await
            .expect("first terminal lease outcome"),
        RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
            if terminal.session_status() == SessionStatus::Completed
    ));
    assert!(matches!(
        repository
            .renew_admitted_run_lease(session_id, AdmissionId::new(), TurnId::new(),)
            .await
            .expect("unrelated terminal lease outcome"),
        RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
    ));
}

fn lease_renewal_waiting_for_writer(
    repository: SqliteSessionRepository,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    now_ms: i64,
    lease_duration_ms: i64,
) -> (
    std::thread::JoinHandle<Result<RunAdmissionLeaseRenewalOutcome, StorageError>>,
    std::sync::mpsc::Sender<()>,
    Arc<std::sync::atomic::AtomicI64>,
) {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;

    thread_local! {
        static WRITER_WAIT: RefCell<Option<(Sender<()>, Receiver<()>)>> =
            const { RefCell::new(None) };
    }

    fn wait_for_writer(_: i32) -> bool {
        WRITER_WAIT.with(|wait| {
            let Some((blocked, resume)) = wait.borrow_mut().take() else {
                return false;
            };
            blocked.send(()).is_ok() && resume.recv_timeout(Duration::from_secs(5)).is_ok()
        })
    }

    let clock = Arc::new(AtomicI64::new(now_ms));
    let renewal_clock = Arc::clone(&clock);
    let (blocked_tx, blocked_rx) = channel();
    let (resume_tx, resume_rx) = channel();
    let worker = std::thread::spawn(move || {
        WRITER_WAIT.with(|wait| *wait.borrow_mut() = Some((blocked_tx, resume_rx)));
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .busy_handler(Some(wait_for_writer))
            .expect("synchronized busy handler");
        let result = repository.renew_admitted_run_lease_with_clock(
            session_id,
            admission_id,
            turn_id,
            lease_duration_ms,
            || renewal_clock.load(Ordering::SeqCst),
        );
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .busy_timeout(Duration::from_secs(5))
            .expect("restore default busy timeout");
        result
    });
    blocked_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("renewal must encounter the held SQLite writer lock");
    (worker, resume_tx, clock)
}

#[tokio::test]
async fn lease_renewal_samples_clock_after_sqlite_writer_wait() {
    use std::sync::atomic::Ordering;

    // The callback runs only after SQLite has encountered the other writer.
    // Advancing the injected clock there avoids a wall-clock sleep or a race
    // between starting the renewal thread and reaching BEGIN IMMEDIATE.
    for after_wait in [1_500, 2_000, 2_001] {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let turn_id = TurnId::new();
        let admission = repository
            .admit_session_turn_at(session_id, turn_id, 1_000, 1_000)
            .await
            .expect("initial admission")
            .expect("admitted owner");
        let before = stored_admission_state(&store, session_id);
        let protocol_before = protocol_turn_table_counts(&store, session_id, turn_id);
        assert_eq!(before.3, Some(2_000));
        let mut writer =
            Connection::open(&store.paths().database_path).expect("independent SQLite connection");
        let transaction = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("hold SQLite writer lock");
        let (worker, resume_tx, clock) = lease_renewal_waiting_for_writer(
            repository,
            session_id,
            admission.admission_id,
            turn_id,
            1_000,
            1_000,
        );
        clock.store(after_wait, Ordering::SeqCst);
        transaction.commit().expect("release SQLite writer lock");
        resume_tx.send(()).expect("resume blocked renewal");
        let outcome = worker.join().expect("renewal worker").expect("renewal");

        let after = stored_admission_state(&store, session_id);
        if after_wait < 2_000 {
            assert!(matches!(outcome, RunAdmissionLeaseRenewalOutcome::Renewed));
            assert_eq!(
                after,
                (before.0, before.1, before.2, Some(after_wait + 1_000)),
                "a fresh owner receives a full lease from the post-wait time"
            );
        } else {
            assert!(matches!(
                outcome,
                RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
            ));
            assert_eq!(after, before, "waiting must not revive an expired owner");
        }
        assert_eq!(
            protocol_turn_table_counts(&store, session_id, turn_id),
            protocol_before,
            "renewal must not append recovery or terminal history"
        );
    }
}

#[tokio::test]
async fn lease_renewal_observes_owner_stop_and_terminal_changes_while_waiting() {
    use std::sync::atomic::Ordering;

    for change in [
        "replacement",
        "interrupt",
        "tree_stop",
        "completed",
        "cancelled",
        "failed",
    ] {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let turn_id = TurnId::new();
        let now = SystemClock::now_ms();
        let lease_duration = 60_000;
        let admission = repository
            .admit_session_turn_at(session_id, turn_id, now, lease_duration)
            .await
            .expect("initial admission")
            .expect("admitted owner");
        let other_store = StoreBundle::new(
            SqliteStore::open(store.paths()).expect("independent repository connection"),
        );
        let other_repository = other_store.session_repo();
        let mut writer =
            Connection::open(&store.paths().database_path).expect("independent SQLite writer");
        let transaction = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("hold SQLite writer lock");
        let (worker, resume_tx, clock) = lease_renewal_waiting_for_writer(
            repository,
            session_id,
            admission.admission_id,
            turn_id,
            now,
            lease_duration,
        );
        transaction.commit().expect("release SQLite writer lock");

        // The renewal is still inside its busy callback. Use a different
        // connection to commit the normal state transition before it retries.
        match change {
            "replacement" => {
                let replacement = other_repository
                    .admit_session_turn_at(
                        session_id,
                        TurnId::new(),
                        now + lease_duration + 1,
                        lease_duration,
                    )
                    .await
                    .expect("recover expired owner")
                    .expect("replacement admission");
                assert_ne!(replacement.admission_id, admission.admission_id);
            }
            "interrupt" => {
                assert!(matches!(
                    other_repository
                        .request_exact_execution_interrupt(
                            session_id,
                            turn_id,
                            admission.admission_revision,
                            crate::protocol::TurnInterruptionCause::UserStop,
                        )
                        .await
                        .expect("request exact Stop"),
                    ExactExecutionInterruptRequestSettlement::Recorded
                ));
            }
            "tree_stop" => {
                other_repository
                    .record_agent_tree_stop_fence_for_expected_state(
                        session_id,
                        crate::protocol::TurnInterruptionCause::UserStop,
                        ActiveTurnExpectation::Turn {
                            turn_id,
                            revision: admission.admission_revision,
                        },
                    )
                    .await
                    .expect("record tree Stop")
                    .expect("exact owner tree fence");
            }
            _ => {
                let terminal = match change {
                    "completed" => completed_terminal_for_response(session_id, None),
                    "cancelled" => RunEvent::TurnTerminal {
                        session_id,
                        terminal: Box::new(pre_admission_user_stopped_terminal()),
                    },
                    "failed" => failed_terminal(session_id, "failure before renewal"),
                    _ => unreachable!("known transition"),
                };
                assert!(matches!(
                    other_repository
                        .settle_admitted_turn_with_protocol_event(
                            session_id,
                            admission.admission_id,
                            &terminal,
                            turn_id,
                            None,
                            None,
                        )
                        .await
                        .expect("terminal before renewal"),
                    AdmittedTerminalSettlement::Applied { .. }
                ));
            }
        }
        clock.store(now + lease_duration + 1, Ordering::SeqCst);
        let expected_state = stored_admission_state(&other_store, session_id);
        let expected_protocol = protocol_turn_table_counts(&other_store, session_id, turn_id);
        let expected_terminal = other_repository
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("terminal before renewal resumes");
        resume_tx.send(()).expect("resume blocked renewal");
        let outcome = worker.join().expect("renewal worker").expect("renewal");
        match change {
            "replacement" => assert!(matches!(
                outcome,
                RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
            )),
            "interrupt" => assert!(matches!(
                outcome,
                RunAdmissionLeaseRenewalOutcome::InterruptRequested(
                    crate::protocol::TurnInterruptionCause::UserStop
                )
            )),
            "tree_stop" => assert!(matches!(
                outcome,
                RunAdmissionLeaseRenewalOutcome::StopFenced(TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                })
            )),
            _ => {
                let RunAdmissionLeaseRenewalOutcome::Terminal(terminal) = outcome else {
                    panic!("{change} must remain terminal: {outcome:?}");
                };
                let expected_status = match change {
                    "completed" => SessionStatus::Completed,
                    "cancelled" => SessionStatus::Cancelled,
                    "failed" => SessionStatus::Failed,
                    _ => unreachable!("known terminal transition"),
                };
                assert_eq!(terminal.session_status(), expected_status);
                assert_eq!(
                    serde_json::to_value(Some(terminal)).expect("renewal terminal"),
                    serde_json::to_value(expected_terminal).expect("winning terminal")
                );
            }
        }
        assert_eq!(stored_admission_state(&store, session_id), expected_state);
        assert_eq!(
            protocol_turn_table_counts(&store, session_id, turn_id),
            expected_protocol,
            "renewal must not overwrite the winning {change} transition"
        );
    }
}

#[tokio::test]
async fn latest_terminal_before_turn_excludes_current_and_uses_append_order() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (first_admission_id, first_turn_id) = active_turn(&store, session_id).await;
    let mut first_terminal = completed_terminal(session_id);
    let RunEvent::TurnTerminal { terminal, .. } = &mut first_terminal else {
        unreachable!("completed terminal helper must be terminal")
    };
    terminal.change_count = 1;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                first_admission_id,
                &first_terminal,
                first_turn_id,
                Some(900),
                None,
            )
            .await
            .expect("complete first turn"),
        AdmittedTerminalCommit::Applied
    );

    let (second_admission_id, second_turn_id) = active_turn(&store, session_id).await;
    let mut second_terminal = completed_terminal(session_id);
    let RunEvent::TurnTerminal { terminal, .. } = &mut second_terminal else {
        unreachable!("completed terminal helper must be terminal")
    };
    terminal.change_count = 2;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                second_admission_id,
                &second_terminal,
                second_turn_id,
                Some(1),
                None,
            )
            .await
            .expect("complete second turn"),
        AdmittedTerminalCommit::Applied
    );

    let (_, current_turn_id) = active_turn(&store, session_id).await;
    let latest = repository
        .latest_durable_terminal_before_turn(session_id, current_turn_id)
        .await
        .expect("latest prior terminal")
        .expect("second terminal");
    assert_eq!(latest.change_count, 2);

    let before_second = repository
        .latest_durable_terminal_before_turn(session_id, second_turn_id)
        .await
        .expect("terminal before second")
        .expect("first terminal");
    assert_eq!(before_second.change_count, 1);
}

#[tokio::test]
async fn retained_terminal_corruption_cannot_be_hidden_or_release_its_owner() {
    let (renew_store, renew_session_id) = test_repo().await;
    let renew_repository = renew_store.session_repo();
    let (renew_admission_id, renew_turn_id) =
        completed_turn_with_retained_admission(&renew_store, renew_session_id).await;
    delete_terminal_runtime_event_for_corruption_test(
        &renew_store,
        renew_session_id,
        renew_turn_id,
    );
    let renew_before = stored_admission_state(&renew_store, renew_session_id);
    let status_error = renew_repository
        .admitted_run_status_at(
            renew_session_id,
            renew_admission_id,
            renew_turn_id,
            SystemClock::now_ms(),
        )
        .await
        .expect_err("single-session reader must reject a missing retained terminal");
    assert!(
        status_error
            .to_string()
            .contains("without a durable terminal")
    );
    let renewal_error = renew_repository
        .renew_admitted_run_lease(renew_session_id, AdmissionId::new(), TurnId::new())
        .await
        .expect_err("wrong caller must not hide a missing retained terminal");
    assert!(
        renewal_error
            .to_string()
            .contains("without a durable terminal")
    );
    assert_eq!(
        stored_admission_state(&renew_store, renew_session_id),
        renew_before
    );
    let wrong_release_error = renew_repository
        .release_stopped_run_admission(renew_session_id, AdmissionId::new())
        .await
        .expect_err("wrong release caller must not hide a missing retained terminal");
    assert!(
        wrong_release_error
            .to_string()
            .contains("without a durable terminal")
    );
    assert_eq!(
        stored_admission_state(&renew_store, renew_session_id),
        renew_before
    );

    let (release_store, release_session_id) = test_repo().await;
    let release_repository = release_store.session_repo();
    let (release_admission_id, release_turn_id) =
        completed_turn_with_retained_admission(&release_store, release_session_id).await;
    delete_terminal_runtime_event_for_corruption_test(
        &release_store,
        release_session_id,
        release_turn_id,
    );
    let release_before = stored_admission_state(&release_store, release_session_id);
    let release_error = release_repository
        .release_stopped_run_admission(release_session_id, release_admission_id)
        .await
        .expect_err("release must validate the retained terminal first");
    assert!(
        release_error
            .to_string()
            .contains("without a durable terminal")
    );
    assert_eq!(
        stored_admission_state(&release_store, release_session_id),
        release_before
    );

    let (recovery_store, recovery_session_id) = test_repo().await;
    let recovery_repository = recovery_store.session_repo();
    let (recovery_admission_id, recovery_turn_id) =
        completed_turn_with_retained_admission(&recovery_store, recovery_session_id).await;
    recovery_repository
        .inject_raw_runtime_state_for_corruption_test(
            recovery_session_id,
            "completed",
            Some(&recovery_admission_id.to_string()),
            Some(&recovery_turn_id.to_string()),
            Some(1),
        )
        .expect("expire retained terminal owner");
    delete_terminal_runtime_event_for_corruption_test(
        &recovery_store,
        recovery_session_id,
        recovery_turn_id,
    );
    let recovery_before = stored_admission_state(&recovery_store, recovery_session_id);
    let replacement_turn_id = TurnId::new();
    let recovery_error = recovery_repository
        .admit_session_turn_at(
            recovery_session_id,
            replacement_turn_id,
            2,
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
        .expect_err("expired recovery must validate the retained terminal first");
    assert!(
        recovery_error
            .to_string()
            .contains("without a durable terminal")
    );
    assert_eq!(
        stored_admission_state(&recovery_store, recovery_session_id),
        recovery_before
    );
    assert!(
        recovery_repository
            .durable_terminal_for_turn(recovery_session_id, replacement_turn_id)
            .await
            .expect("replacement terminal lookup")
            .is_none()
    );
    let same_turn_error = recovery_repository
        .admit_session_turn_at(
            recovery_session_id,
            recovery_turn_id,
            2,
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
        .expect_err("corrupt retained identity must not become reusable");
    assert!(
        same_turn_error
            .to_string()
            .contains("without a durable terminal")
    );
    assert_eq!(
        stored_admission_state(&recovery_store, recovery_session_id),
        recovery_before
    );

    let (mismatch_store, mismatch_session_id) = test_repo().await;
    let mismatch_repository = mismatch_store.session_repo();
    let (mismatch_admission_id, mismatch_turn_id) =
        completed_turn_with_retained_admission(&mismatch_store, mismatch_session_id).await;
    let mismatch_lease = stored_admission_state(&mismatch_store, mismatch_session_id)
        .3
        .expect("retained lease");
    mismatch_repository
        .inject_raw_runtime_state_for_corruption_test(
            mismatch_session_id,
            "failed",
            Some(&mismatch_admission_id.to_string()),
            Some(&mismatch_turn_id.to_string()),
            Some(mismatch_lease),
        )
        .expect("inject terminal status mismatch");
    let mismatch_before = stored_admission_state(&mismatch_store, mismatch_session_id);
    let mismatch_status_error = mismatch_repository
        .admitted_run_status_at(
            mismatch_session_id,
            mismatch_admission_id,
            mismatch_turn_id,
            SystemClock::now_ms(),
        )
        .await
        .expect_err("single-session reader must reject a terminal status mismatch");
    assert!(
        mismatch_status_error
            .to_string()
            .contains("contradicts durable terminal status")
    );
    let mismatch_error = mismatch_repository
        .renew_admitted_run_lease(mismatch_session_id, AdmissionId::new(), TurnId::new())
        .await
        .expect_err("wrong caller must not hide a terminal status mismatch");
    assert!(
        mismatch_error
            .to_string()
            .contains("contradicts durable terminal status")
    );
    assert_eq!(
        stored_admission_state(&mismatch_store, mismatch_session_id),
        mismatch_before
    );
    let mismatch_release_error = mismatch_repository
        .release_stopped_run_admission(mismatch_session_id, mismatch_admission_id)
        .await
        .expect_err("release must reject a terminal status mismatch");
    assert!(
        mismatch_release_error
            .to_string()
            .contains("contradicts durable terminal status")
    );
    assert_eq!(
        stored_admission_state(&mismatch_store, mismatch_session_id),
        mismatch_before
    );
    mismatch_repository
        .inject_raw_runtime_state_for_corruption_test(
            mismatch_session_id,
            "failed",
            Some(&mismatch_admission_id.to_string()),
            Some(&mismatch_turn_id.to_string()),
            Some(1),
        )
        .expect("expire mismatched terminal owner");
    let expired_mismatch_before = stored_admission_state(&mismatch_store, mismatch_session_id);
    let mismatch_recovery_error = mismatch_repository
        .admit_session_turn_at(
            mismatch_session_id,
            TurnId::new(),
            2,
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
        .expect_err("expired recovery must reject a terminal status mismatch");
    assert!(
        mismatch_recovery_error
            .to_string()
            .contains("contradicts durable terminal status")
    );
    assert_eq!(
        stored_admission_state(&mismatch_store, mismatch_session_id),
        expired_mismatch_before
    );
}

#[tokio::test]
async fn duplicate_retained_terminal_blocks_renewal_release_and_recovery() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = completed_turn_with_retained_admission(&store, session_id).await;
    inject_duplicate_terminal_runtime_event_for_corruption_test(&store, session_id, turn_id);
    let before = stored_admission_state(&store, session_id);

    let status_error = repository
        .admitted_run_status_at(session_id, admission_id, turn_id, SystemClock::now_ms())
        .await
        .expect_err("single-session reader must reject duplicate terminals");
    assert!(
        status_error
            .to_string()
            .contains("multiple durable terminals")
    );

    let renewal_error = repository
        .renew_admitted_run_lease(session_id, AdmissionId::new(), TurnId::new())
        .await
        .expect_err("duplicate terminal must be detected before owner comparison");
    assert!(
        renewal_error
            .to_string()
            .contains("multiple durable terminals")
    );
    assert_eq!(stored_admission_state(&store, session_id), before);

    let release_error = repository
        .release_stopped_run_admission(session_id, admission_id)
        .await
        .expect_err("duplicate terminal must block owner release");
    assert!(
        release_error
            .to_string()
            .contains("multiple durable terminals")
    );
    assert_eq!(stored_admission_state(&store, session_id), before);

    repository
        .inject_raw_runtime_state_for_corruption_test(
            session_id,
            "completed",
            Some(&admission_id.to_string()),
            Some(&turn_id.to_string()),
            Some(1),
        )
        .expect("expire duplicate terminal owner");
    let expired_before = stored_admission_state(&store, session_id);
    let recovery_error = repository
        .admit_session_turn_at(
            session_id,
            TurnId::new(),
            2,
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
        .expect_err("duplicate terminal must block expired-owner recovery");
    assert!(
        recovery_error
            .to_string()
            .contains("multiple durable terminals")
    );
    assert_eq!(stored_admission_state(&store, session_id), expired_before);
}

#[tokio::test]
async fn valid_retained_terminal_can_be_observed_released_or_replaced() {
    let (release_store, release_session_id) = test_repo().await;
    let release_repository = release_store.session_repo();
    let (release_admission_id, release_turn_id) =
        completed_turn_with_retained_admission(&release_store, release_session_id).await;
    assert!(matches!(
        release_repository
            .renew_admitted_run_lease(
                release_session_id,
                release_admission_id,
                release_turn_id,
            )
            .await
            .expect("typed retained terminal"),
        RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
            if terminal.session_status() == SessionStatus::Completed
    ));
    assert!(
        release_repository
            .release_stopped_run_admission(release_session_id, release_admission_id)
            .await
            .expect("release valid retained terminal")
    );
    assert_eq!(
        stored_admission_state(&release_store, release_session_id),
        ("completed".to_string(), None, None, None)
    );

    let (replace_store, replace_session_id) = test_repo().await;
    let replace_repository = replace_store.session_repo();
    let (replace_admission_id, replace_turn_id) =
        completed_turn_with_retained_admission(&replace_store, replace_session_id).await;
    replace_repository
        .inject_raw_runtime_state_for_corruption_test(
            replace_session_id,
            "completed",
            Some(&replace_admission_id.to_string()),
            Some(&replace_turn_id.to_string()),
            Some(1),
        )
        .expect("expire valid retained terminal owner");
    let replacement_turn_id = TurnId::new();
    let replacement = replace_repository
        .admit_session_turn_at(
            replace_session_id,
            replacement_turn_id,
            2,
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
        .expect("replace valid expired retained owner")
        .expect("replacement admission");
    let replaced_state = stored_admission_state(&replace_store, replace_session_id);
    assert_eq!(replaced_state.0, "running");
    assert_eq!(replaced_state.1, Some(replacement.admission_id.to_string()));
    assert_eq!(replaced_state.2, Some(replacement_turn_id.to_string()));
}

#[tokio::test]
async fn running_session_with_a_terminal_is_reported_as_corrupt() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let project_id = repository
        .get_session(session_id)
        .await
        .expect("session before corruption")
        .project_id;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let terminal_event = completed_terminal(session_id);
    let projection = project_protocol_run_event(&terminal_event, Some(session_id), turn_id, 1)
        .expect("terminal projection");
    {
        let mut connection = repository.connection.lock().expect("sqlite mutex");
        let transaction = connection.transaction().expect("transaction");
        insert_session_owned_event_bundle_in_transaction(
            &SESSION_PROTOCOL_WRITE_AUTHORITY,
            &transaction,
            &projection.runtime_event,
            projection.history_item.as_ref(),
            projection.turn_item.as_ref(),
        )
        .expect("inject terminal before status CAS");
        transaction.commit().expect("commit corrupt fixture");
    }
    let corrupt_state = stored_admission_state(&store, session_id);
    let protocol_before = (
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history before rejected write")
            .len(),
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("runtime before rejected write")
            .len(),
        store
            .protocol_event_store()
            .list_turn_items(session_id, turn_id)
            .expect("turn items before rejected write")
            .len(),
    );

    let response_error = repository
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id: ModelResponseId::new(),
                assistant_text: Some("must not commit after terminal".to_string()),
                assistant_protocol_sequence_no: None,
                tool_calls: Vec::new(),
            },
        )
        .await
        .expect_err("active-admission writer must reject running plus terminal corruption");
    assert!(
        response_error
            .to_string()
            .contains("already has a durable terminal")
    );
    let protocol_after = (
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("history after rejected write")
            .len(),
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("runtime after rejected write")
            .len(),
        store
            .protocol_event_store()
            .list_turn_items(session_id, turn_id)
            .expect("turn items after rejected write")
            .len(),
    );
    assert_eq!(protocol_after, protocol_before);
    assert!(repository.get_session(session_id).await.is_err());
    assert!(
        repository
            .active_session_for_project(project_id)
            .await
            .is_err()
    );
    assert!(
        repository
            .mutation_blocker_in_session_tree(session_id)
            .await
            .is_err()
    );

    let admission_error = repository
        .admit_session_turn(session_id, TurnId::new())
        .await
        .expect_err("running terminal must fail admission integrity checks");
    assert!(
        admission_error
            .to_string()
            .contains("already has a durable terminal")
    );
    assert_eq!(stored_admission_state(&store, session_id), corrupt_state);

    let renewal_error = repository
        .renew_admitted_run_lease(session_id, admission_id, turn_id)
        .await
        .expect_err("running terminal must fail renewal integrity checks");
    assert!(
        renewal_error
            .to_string()
            .contains("already has a durable terminal")
    );
    let release_error = repository
        .release_stopped_run_admission(session_id, admission_id)
        .await
        .expect_err("running terminal must fail release integrity checks");
    assert!(
        release_error
            .to_string()
            .contains("already has a durable terminal")
    );
    assert_eq!(stored_admission_state(&store, session_id), corrupt_state);
    let terminal_error = repository
        .terminalize_admitted_turn_with_protocol_event(
            session_id,
            admission_id,
            &terminal_event,
            turn_id,
            None,
            None,
        )
        .await
        .expect_err("running terminal must not masquerade as an idempotent commit");
    assert!(
        terminal_error
            .to_string()
            .contains("already has a durable terminal")
    );
}

#[tokio::test]
async fn terminal_reader_rejects_multiple_rows_even_if_the_index_is_corrupted() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &completed_terminal(session_id),
                turn_id,
                None,
                None,
            )
            .await
            .expect("first terminal"),
        AdmittedTerminalCommit::Applied
    );
    let duplicate = project_protocol_run_event(
        &failed_terminal(session_id, "duplicate terminal"),
        Some(session_id),
        turn_id,
        100,
    )
    .expect("duplicate projection")
    .runtime_event;
    let duplicate_json = serde_json::to_string(&duplicate.msg).expect("duplicate JSON");
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute_batch("DROP INDEX idx_protocol_runtime_events_unique_turn_terminal")
            .expect("remove index for corruption fixture");
        connection
            .execute(
                "INSERT INTO protocol_runtime_events
                     (id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'corrupt-fixture', ?6)",
                params![
                    duplicate.id.to_string(),
                    session_id.to_string(),
                    turn_id.to_string(),
                    duplicate.sequence_no,
                    duplicate_json,
                    duplicate.created_at_ms,
                ],
            )
            .expect("inject duplicate terminal");
    }

    let error = repository
        .durable_terminal_for_turn(session_id, turn_id)
        .await
        .expect_err("duplicate durable terminal must fail closed");
    assert!(error.to_string().contains("multiple durable terminals"));
}

#[tokio::test]
async fn initial_user_turn_validation_is_side_effect_free() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let admission_turn_id = TurnId::new();
    let mismatched_turn = text_user_turn(TurnId::new(), "mismatched");

    let mismatch = repository
        .admit_session_turn_with_initial_user_turn(
            session_id,
            admission_turn_id,
            Some(&mismatched_turn),
        )
        .await
        .expect_err("mismatched user turn must be rejected");
    assert!(mismatch.to_string().contains("identity mismatch"));

    let empty_turn = UserTurn {
        turn_id: admission_turn_id,
        items: vec![UserInputItem::Text {
            text: " \n ".to_string(),
        }],
        prompt_dispatch: None,
        editor_context: None,
    };
    let empty = repository
        .admit_session_turn_with_initial_user_turn(session_id, admission_turn_id, Some(&empty_turn))
        .await
        .expect_err("empty user turn must be rejected");
    assert!(empty.to_string().contains("must contain text or an image"));

    assert_eq!(
        stored_admission_state(&store, session_id),
        ("idle".to_string(), None, None, None)
    );
    assert!(
        protocol_turn_table_counts(&store, session_id, admission_turn_id)
            .iter()
            .all(|(_, count)| *count == 0)
    );
}

#[tokio::test]
async fn admission_bundle_rolls_back_session_goal_protocol_and_allocator_on_projection_failure() {
    for failure_point in ["session_started", "user_turn"] {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let turn_id = TurnId::new();
        let user_turn = text_user_turn(turn_id, "atomic request");
        let trigger = match failure_point {
            "session_started" => {
                "CREATE TRIGGER abort_initial_session_started
                     BEFORE INSERT ON protocol_runtime_events
                     WHEN NEW.sequence_no = 0
                     BEGIN SELECT RAISE(ABORT, 'injected SessionStarted failure'); END;"
            }
            "user_turn" => {
                "CREATE TRIGGER abort_initial_user_turn
                     BEFORE INSERT ON protocol_history_items
                     WHEN json_extract(NEW.payload_json, '$.kind') = 'user_turn'
                     BEGIN SELECT RAISE(ABORT, 'injected UserTurnStored failure'); END;"
            }
            _ => unreachable!("known failure point"),
        };
        repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(trigger)
            .expect("failure trigger");

        repository
            .admit_session_turn_with_goal_objective_and_initial_user_turn(
                session_id,
                turn_id,
                "must roll back",
                Some(&user_turn),
            )
            .await
            .expect_err("injected projection failure");

        assert_eq!(
            stored_admission_state(&store, session_id),
            ("idle".to_string(), None, None, None),
            "{failure_point} retained the session admission"
        );
        assert!(
            repository
                .get_thread_goal(session_id)
                .await
                .expect("goal after rollback")
                .is_none(),
            "{failure_point} retained the goal mutation"
        );
        for (table, count) in protocol_turn_table_counts(&store, session_id, turn_id) {
            assert_eq!(
                count, 0,
                "{failure_point} retained a row in {table} after rollback"
            );
        }
    }
}

#[tokio::test]
async fn image_only_initial_user_turn_is_committed_with_the_admission() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let turn_id = TurnId::new();
    let user_turn = UserTurn {
        turn_id,
        items: vec![UserInputItem::Image {
            image: crate::session::ImagePart {
                source_path: None,
                mime_type: "image/png".to_string(),
                data_base64: "iVBORw0KGgo=".to_string(),
                byte_len: 8,
            },
        }],
        prompt_dispatch: None,
        editor_context: None,
    };

    repository
        .admit_session_turn_with_initial_user_turn(session_id, turn_id, Some(&user_turn))
        .await
        .expect("image-only admission")
        .expect("image-only turn admitted");

    let events = store
        .protocol_event_store()
        .list_runtime_events(session_id, turn_id)
        .expect("runtime events");
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence_no)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let history = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history");
    assert!(matches!(
        history.as_slice(),
        [HistoryItem {
            payload: HistoryItemPayload::UserTurn { content, .. },
            ..
        }] if matches!(content.as_slice(), [ContentPart::Image { .. }])
    ));
}

#[tokio::test]
async fn concurrent_admission_commits_exactly_one_run_and_turn_owner() {
    let (store, session_id) = test_repo().await;
    let first_repository = store.session_repo();
    let second_repository = store.session_repo();
    let first_turn_id = TurnId::new();
    let second_turn_id = TurnId::new();
    let first_user_turn = text_user_turn(first_turn_id, "first");
    let second_user_turn = text_user_turn(second_turn_id, "second");
    let (first, second) = tokio::join!(
        first_repository.admit_session_turn_with_initial_user_turn(
            session_id,
            first_turn_id,
            Some(&first_user_turn),
        ),
        second_repository.admit_session_turn_with_initial_user_turn(
            session_id,
            second_turn_id,
            Some(&second_user_turn),
        ),
    );
    let first = first.expect("first admission attempt");
    let second = second.expect("second admission attempt");
    let (winning_admission_id, winning_turn_id) = match (first, second) {
        (Some(admission), None) => (admission.admission_id, first_turn_id),
        (None, Some(admission)) => (admission.admission_id, second_turn_id),
        outcome => panic!("expected one admitted turn, got {outcome:?}"),
    };

    let state = stored_admission_state(&store, session_id);
    assert_eq!(state.0, "running");
    assert_eq!(state.1, Some(winning_admission_id.to_string()));
    assert_eq!(state.2, Some(winning_turn_id.to_string()));
    assert!(state.3.is_some());
    let losing_turn_id = if winning_turn_id == first_turn_id {
        second_turn_id
    } else {
        first_turn_id
    };
    assert_eq!(
        protocol_turn_table_counts(&store, session_id, winning_turn_id),
        vec![
            ("protocol_runtime_events", 2),
            ("protocol_history_items", 1),
            ("protocol_turn_items", 1),
            ("protocol_item_append_order", 4),
            ("protocol_turn_sequence_allocators", 1),
        ],
        "the winner must own the complete admission bundle"
    );
    assert!(
        protocol_turn_table_counts(&store, session_id, losing_turn_id)
            .iter()
            .all(|(_, count)| *count == 0),
        "the losing turn must leave no partial bundle"
    );
}

#[tokio::test]
async fn goal_change_admission_captures_one_immutable_goal_owner() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    repository
        .replace_thread_goal(
            session_id,
            "old objective",
            ThreadGoalStatus::Active,
            Some(100),
        )
        .await
        .expect("initial goal");
    let (_, original_goal_id) = repository
        .get_thread_goal_with_id(session_id)
        .await
        .expect("read initial goal")
        .expect("stored initial goal");

    let turn_id = TurnId::new();
    let initial_user_turn = text_user_turn(turn_id, "admitted objective");
    let admission = repository
        .admit_session_turn_with_goal_objective_and_initial_user_turn(
            session_id,
            turn_id,
            "admitted objective",
            Some(&initial_user_turn),
        )
        .await
        .expect("atomic admission")
        .expect("turn admitted");
    let captured = admission.goal.expect("captured goal");
    assert_eq!(captured.goal_id, original_goal_id);
    assert_eq!(captured.goal.objective, "admitted objective");
    assert_eq!(captured.goal.status, ThreadGoalStatus::Active);

    repository
        .replace_thread_goal(
            session_id,
            "replacement after admission",
            ThreadGoalStatus::Active,
            None,
        )
        .await
        .expect("replace after admission");
    let (replacement, replacement_goal_id) = repository
        .get_thread_goal_with_id(session_id)
        .await
        .expect("replacement read")
        .expect("replacement goal");
    assert_ne!(replacement_goal_id, captured.goal_id);
    assert_eq!(captured.goal.objective, "admitted objective");

    repository
        .account_thread_goal_usage_for_goal(session_id, 25, Some(captured.goal_id.as_str()))
        .await
        .expect("stale captured usage is ignored");
    let current = repository
        .get_thread_goal(session_id)
        .await
        .expect("current goal")
        .expect("current goal exists");
    assert_eq!(current.objective, replacement.objective);
    assert_eq!(current.tokens_used, 0);
}

#[tokio::test]
async fn exact_idle_goal_set_accounts_time_and_updates_the_captured_owner_atomically() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let turn_a = TurnId::new();
    let admitted_a = repository
        .admit_session_turn(session_id, turn_a)
        .await
        .expect("admit A")
        .expect("A owner");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admitted_a.admission_id,
                &completed_terminal(session_id),
                turn_a,
                None,
                None,
            )
            .await
            .expect("terminalize A"),
        AdmittedTerminalCommit::Applied
    );

    let created = repository
        .set_thread_goal_at_idle_expectation(
            session_id,
            Some("captured A objective"),
            Some(ThreadGoalStatus::Active),
            Some(Some(100)),
            Some(turn_a),
            admitted_a.admission_revision,
        )
        .await
        .expect("create exact Idle(A) goal")
        .expect("created goal");
    assert_eq!(created.objective, "captured A objective");
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute(
                "UPDATE thread_goals
                     SET tokens_used = 7,
                         time_used_seconds = 3,
                         updated_at_ms = updated_at_ms - 3000
                     WHERE thread_id = ?1",
                [session_id.to_string()],
            )
            .expect("age exact goal accounting clock");
    }

    let updated = repository
        .set_thread_goal_at_idle_expectation(
            session_id,
            Some("updated at Idle(A)"),
            Some(ThreadGoalStatus::Paused),
            Some(Some(50)),
            Some(turn_a),
            admitted_a.admission_revision,
        )
        .await
        .expect("update exact Idle(A) goal")
        .expect("updated goal");
    assert_eq!(updated.objective, "updated at Idle(A)");
    assert_eq!(updated.status, ThreadGoalStatus::Paused);
    assert_eq!(updated.token_budget, Some(50));
    assert_eq!(updated.tokens_used, 7);
    assert!(
        updated.time_used_seconds >= 6,
        "elapsed active-goal time must be accounted before the exact mutation"
    );
}

#[tokio::test]
async fn stale_idle_goal_set_cannot_cross_turn_replacement_or_revision_drift() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let turn_a = TurnId::new();
    let admitted_a = repository
        .admit_session_turn(session_id, turn_a)
        .await
        .expect("admit A")
        .expect("A owner");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admitted_a.admission_id,
                &completed_terminal(session_id),
                turn_a,
                None,
                None,
            )
            .await
            .expect("terminalize A"),
        AdmittedTerminalCommit::Applied
    );
    let original = repository
        .set_thread_goal_at_idle_expectation(
            session_id,
            Some("stable goal"),
            Some(ThreadGoalStatus::Active),
            Some(Some(100)),
            Some(turn_a),
            admitted_a.admission_revision,
        )
        .await
        .expect("create stable goal")
        .expect("stable goal exists");

    let turn_b = TurnId::new();
    let admitted_b = repository
        .admit_session_turn(session_id, turn_b)
        .await
        .expect("admit replacement B")
        .expect("B owner");
    assert!(
        repository
            .set_thread_goal_at_idle_expectation(
                session_id,
                Some("must not cross running B"),
                Some(ThreadGoalStatus::Paused),
                None,
                Some(turn_a),
                admitted_a.admission_revision,
            )
            .await
            .is_err()
    );
    assert_eq!(
        repository
            .get_thread_goal(session_id)
            .await
            .expect("goal after running B")
            .expect("goal remains")
            .objective,
        original.objective
    );

    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admitted_b.admission_id,
                &completed_terminal(session_id),
                turn_b,
                None,
                None,
            )
            .await
            .expect("terminalize B"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        repository
            .set_thread_goal_at_idle_expectation(
                session_id,
                Some("must not cross Idle(B)"),
                None,
                None,
                Some(turn_a),
                admitted_a.admission_revision,
            )
            .await
            .is_err()
    );
    repository
        .rollback_session_transaction(session_id, 1)
        .await
        .expect("rollback B to latest A with a new revision");
    assert!(
        repository
            .set_thread_goal_at_idle_expectation(
                session_id,
                Some("must not cross revision drift"),
                None,
                None,
                Some(turn_a),
                admitted_a.admission_revision,
            )
            .await
            .is_err()
    );
    let unchanged = repository
        .get_thread_goal(session_id)
        .await
        .expect("goal after stale exact mutations")
        .expect("goal remains after stale exact mutations");
    assert_eq!(unchanged.objective, original.objective);
    assert_eq!(unchanged.status, original.status);
    assert_eq!(unchanged.token_budget, original.token_budget);
    assert_eq!(unchanged.tokens_used, original.tokens_used);
}

#[tokio::test]
async fn active_goal_continuation_admission_is_atomic_and_inactive_is_side_effect_free() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let predecessor_turn_id = TurnId::new();
    let predecessor = repository
        .admit_session_turn(session_id, predecessor_turn_id)
        .await
        .expect("predecessor admission")
        .expect("predecessor owner");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                predecessor.admission_id,
                &completed_terminal(session_id),
                predecessor_turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize predecessor"),
        AdmittedTerminalCommit::Applied
    );
    repository
        .replace_thread_goal(
            session_id,
            "continue until verified",
            ThreadGoalStatus::Active,
            None,
        )
        .await
        .expect("active goal");

    let admitted_turn_id = TurnId::new();
    let initial_user_turn = text_user_turn(admitted_turn_id, "continue the goal");
    let admitted = match repository
        .admit_active_goal_continuation_turn_with_initial_user_turn(
            session_id,
            admitted_turn_id,
            Some(&initial_user_turn),
            predecessor_turn_id,
            predecessor.admission_revision,
        )
        .await
        .expect("active-goal admission")
    {
        ActiveGoalTurnAdmission::Admitted(snapshot) => snapshot,
        outcome => panic!("active goal was not admitted: {outcome:?}"),
    };
    assert_eq!(
        admitted.goal.as_ref().map(|goal| goal.goal.status),
        Some(ThreadGoalStatus::Active)
    );
    let terminal = completed_terminal(session_id);
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admitted.admission_id,
                &terminal,
                admitted_turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize continuation"),
        AdmittedTerminalCommit::Applied
    );

    repository
        .update_thread_goal(session_id, None, Some(ThreadGoalStatus::Paused), None)
        .await
        .expect("pause goal")
        .expect("goal retained");
    let before = stored_admission_state(&store, session_id);
    assert!(matches!(
        repository
            .admit_active_goal_continuation_turn(
                session_id,
                TurnId::new(),
                admitted_turn_id,
                admitted.admission_revision,
            )
            .await
            .expect("inactive-goal admission"),
        ActiveGoalTurnAdmission::GoalInactive
    ));
    assert_eq!(stored_admission_state(&store, session_id), before);
    assert!(
        !repository
            .has_fresh_run_admission(session_id)
            .await
            .expect("no inactive-goal admission")
    );
}

#[tokio::test]
async fn terminal_root_stop_fence_blocks_the_exact_goal_continuation_predecessor() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    repository
        .replace_thread_goal(
            session_id,
            "must stop before continuation",
            ThreadGoalStatus::Active,
            None,
        )
        .await
        .expect("active goal");
    let turn_a = TurnId::new();
    let admission_a = repository
        .admit_session_turn(session_id, turn_a)
        .await
        .expect("A admission")
        .expect("A owner");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_a.admission_id,
                &completed_terminal(session_id),
                turn_a,
                None,
                None,
            )
            .await
            .expect("terminalize A"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        repository
            .record_agent_tree_stop_fence_for_expected_state(
                session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
                ActiveTurnExpectation::Turn {
                    turn_id: turn_a,
                    revision: admission_a.admission_revision,
                },
            )
            .await
            .expect("Stop A fence")
            .is_some()
    );

    let turn_b = TurnId::new();
    assert!(matches!(
        repository
            .admit_active_goal_continuation_turn(
                session_id,
                turn_b,
                turn_a,
                admission_a.admission_revision,
            )
            .await
            .expect("B continuation decision"),
        ActiveGoalTurnAdmission::Unavailable
    ));
    assert!(
        repository
            .delete_thread_goal(session_id)
            .await
            .expect("remove goal before mailbox-style continuation")
    );
    assert!(
        matches!(
            repository
                .admit_root_continuation_turn_with_initial_user_turn(
                    session_id,
                    TurnId::new(),
                    None,
                    turn_a,
                    admission_a.admission_revision,
                    false,
                )
                .await
                .expect("goal-independent root continuation decision"),
            ActiveGoalTurnAdmission::Unavailable
        ),
        "the predecessor tree-Stop fence must also block mailbox-driven root continuation"
    );
    assert_eq!(
        repository
            .active_turn_expectation_for_session(session_id)
            .await
            .expect("post-Stop expectation"),
        Some(ActiveTurnExpectation::Idle {
            latest_turn_id: Some(turn_a),
            revision: admission_a.admission_revision,
        })
    );

    let user_turn = TurnId::new();
    assert!(
        repository
            .admit_session_turn_with_initial_user_turn_after_latest(
                session_id,
                user_turn,
                None,
                Some(turn_a),
                admission_a.admission_revision,
            )
            .await
            .expect("new user run decision")
            .is_some(),
        "a new top-level user run is not part of the stopped root scope"
    );
}

#[tokio::test]
async fn rejected_goal_change_admission_does_not_mutate_goal() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    repository
        .replace_thread_goal(
            session_id,
            "owned objective",
            ThreadGoalStatus::Active,
            None,
        )
        .await
        .expect("initial goal");
    repository
        .admit_session_turn(session_id, TurnId::new())
        .await
        .expect("first admission")
        .expect("first owner");

    assert!(
            repository
                .admit_session_turn_with_goal_objective(
                    session_id,
                    TurnId::new(),
                    "must not be stored",
                )
                .await
                .expect("rejected admission")
                .is_none()
        );
    let goal = repository
        .get_thread_goal(session_id)
        .await
        .expect("goal read")
        .expect("goal retained");
    assert_eq!(goal.objective, "owned objective");
}

#[tokio::test]
async fn expired_owner_is_recovered_before_atomic_replacement_admission() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let admitted_at_ms = SystemClock::now_ms();
    let expired_turn_id = TurnId::new();
    let expired_admission_id = repository
        .admit_session_turn_at(session_id, expired_turn_id, admitted_at_ms, 100)
        .await
        .expect("expired owner setup")
        .expect("expired owner admitted");
    let replacement_turn_id = TurnId::new();
    let replacement_admission_id = repository
        .admit_session_turn_at(
            session_id,
            replacement_turn_id,
            admitted_at_ms.saturating_add(101),
            RUN_ADMISSION_LEASE_DURATION_MS,
        )
        .await
        .expect("replacement admission")
        .expect("replacement admitted");

    let state = stored_admission_state(&store, session_id);
    assert_eq!(state.0, "running");
    assert_eq!(
        state.1,
        Some(replacement_admission_id.admission_id.to_string())
    );
    assert_eq!(state.2, Some(replacement_turn_id.to_string()));
    assert!(matches!(
        repository
            .renew_admitted_run_lease_at(
                session_id,
                expired_admission_id.admission_id,
                expired_turn_id,
                admitted_at_ms.saturating_add(102),
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect("stale owner renewal"),
        RunAdmissionLeaseRenewalOutcome::SupersededOrExpired
    ));
    assert_eq!(
        repository
            .durable_terminal_for_turn(session_id, expired_turn_id)
            .await
            .expect("recovery terminal")
            .map(|terminal| terminal.session_status()),
        Some(SessionStatus::Failed)
    );
}

#[tokio::test]
async fn expired_owner_replacement_settles_queued_steer_and_remains_migratable() {
    for stopped in [false, true] {
        let (store, session_id) = test_repo().await;
        let repository = store.session_repo();
        let admitted_at_ms = SystemClock::now_ms();
        let expired_turn_id = TurnId::new();
        repository
            .admit_session_turn_at(
                session_id,
                expired_turn_id,
                admitted_at_ms,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect("old admission")
            .expect("old owner admitted");
        let input_id = repository
            .accept_active_turn_steer(
                session_id,
                1,
                &SteerTurn {
                    expected_turn_id: expired_turn_id,
                    items: vec![UserInputItem::Text {
                        text: "preserve the accepted input".to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("queued-before-expiry".to_string()),
                },
            )
            .await
            .expect("queue steer");
        if stopped {
            repository
                .record_agent_tree_stop_fence(
                    session_id,
                    crate::protocol::TurnInterruptionCause::UserStop,
                )
                .await
                .expect("Stop boundary")
                .expect("Stop fence recorded");
        }
        repository
            .admit_session_turn_at(
                session_id,
                TurnId::new(),
                admitted_at_ms + RUN_ADMISSION_LEASE_DURATION_MS + 1,
                RUN_ADMISSION_LEASE_DURATION_MS,
            )
            .await
            .expect("replace expired owner")
            .expect("replacement admitted");

        crate::storage::SqliteStore::open(store.paths())
            .expect("reopen database")
            .migrate()
            .expect("recovered queue must remain valid at startup");
        let history = store
            .protocol_event_store()
            .list_history_items(session_id, expired_turn_id)
            .expect("expired turn history");
        assert_eq!(
            history.iter().filter(|item| item.id == input_id).count(),
            usize::from(!stopped)
        );
        let state = repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT state FROM turn_steer_inputs WHERE id = ?1",
                [input_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .expect("queue state");
        assert_eq!(state, if stopped { "discarded" } else { "delivered" });
        assert_eq!(
            repository
                .durable_terminal_for_turn(session_id, expired_turn_id)
                .await
                .expect("recovered terminal")
                .expect("terminal")
                .session_status(),
            if stopped {
                SessionStatus::Cancelled
            } else {
                SessionStatus::Failed
            },
        );
    }
}

#[tokio::test]
async fn admitted_user_turn_is_the_only_durable_message_contract() {
    let (store, session_id) = test_repo().await;
    let (_, turn_id) = active_turn(&store, session_id).await;
    let history = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history");
    assert!(matches!(
        history.as_slice(),
        [HistoryItem {
            payload: HistoryItemPayload::UserTurn { content, .. },
            ..
        }] if matches!(content.as_slice(), [ContentPart::Text { text }] if text == "canonical request")
    ));
    let repo = store.session_repo();
    let connection = repo.connection.lock().expect("sqlite mutex");
    for retired in ["messages", "message_parts"] {
        let exists = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![retired],
                |row| row.get::<_, bool>(0),
            )
            .expect("schema query");
        assert!(!exists, "retired table {retired} must not exist after V33");
    }
}

async fn record_progress_shell_calls(
    store: &StoreBundle,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    call_ids: &[ToolCallId],
) {
    store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id: ModelResponseId::new(),
                assistant_text: None,
                assistant_protocol_sequence_no: None,
                tool_calls: call_ids
                    .iter()
                    .enumerate()
                    .map(|(index, id)| PendingToolCallWrite {
                        id: *id,
                        model_call_id: format!("progress-call-{index}"),
                        tool_name: "shell".into(),
                        arguments_json: serde_json::json!({"command": "python -m unittest"})
                            .to_string(),
                        protocol_sequence_no: None,
                    })
                    .collect(),
            },
        )
        .await
        .expect("pending progress calls");
}

#[tokio::test]
async fn canonical_turn_progress_counts_display_outcomes_beyond_page_and_latest_turn() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (older_admission, older_turn) = active_turn(&store, session_id).await;
    let older_call = ToolCallId::new();
    record_progress_shell_calls(
        &store,
        session_id,
        older_admission,
        older_turn,
        &[older_call],
    )
    .await;
    repository
        .fail_tool_call_with_protocol_bundle(
            session_id,
            older_admission,
            older_call,
            crate::tool::ToolName::Shell,
            "older invocation failed",
            serde_json::json!({}),
            older_turn,
            None,
        )
        .await
        .expect("older typed failure")
        .expect("older call settled");
    let mut older_terminal = completed_terminal_for_response(session_id, None);
    if let RunEvent::TurnTerminal { terminal, .. } = &mut older_terminal {
        terminal.tool_call_count = 1;
        terminal.failed_tool_count = 1;
    }
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                older_admission,
                &older_terminal,
                older_turn,
                None,
                None,
            )
            .await
            .expect("older terminal"),
        AdmittedTerminalCommit::Applied
    );

    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let calls = [ToolCallId::new(), ToolCallId::new(), ToolCallId::new()];
    record_progress_shell_calls(&store, session_id, admission_id, turn_id, &calls).await;
    repository
            .complete_tool_call_with_protocol_bundle(
                session_id,
                admission_id,
                calls[0],
                crate::tool::ToolName::Shell,
                "unsuccessful operation",
                serde_json::json!({"success": true, "tool_metadata": {"success": false, "exit_code": 7}}),
                "Exit code: 7",
                None,
                turn_id,
                None,
            )
            .await
            .expect("non-success completion")
            .expect("non-success call settled");
    let initial = repository
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Latest { limit: 1 },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await
        .expect("bounded active snapshot");
    assert_eq!(initial.protocol.history.items.len(), 1);
    assert_eq!(initial.protocol.turns.items.len(), 1);
    let initial_progress = initial.turn_progress.expect("active display aggregate");
    assert_eq!(initial_progress.turn_id, turn_id);
    assert_eq!(initial_progress.tool_call_count, 3);
    assert_eq!(initial_progress.completed_tool_count, 0);
    assert_eq!(initial_progress.failed_tool_count, 1);
    assert!(
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("non-success runtime evidence")
            .iter()
            .any(|event| matches!(
                &event.msg,
                RuntimeEventMsg::ToolLifecycle { envelope }
                    if envelope.call_id == calls[0]
                        && envelope.status == ToolLifecycleStatus::Completed
                        && envelope.success == Some(false)
            ))
    );

    repository
        .fail_tool_call_with_protocol_bundle(
            session_id,
            admission_id,
            calls[1],
            crate::tool::ToolName::Shell,
            "typed invocation failure",
            serde_json::json!({}),
            turn_id,
            None,
        )
        .await
        .expect("typed failure")
        .expect("failed call settled");
    repository
        .complete_tool_call_with_protocol_bundle(
            session_id,
            admission_id,
            calls[2],
            crate::tool::ToolName::Shell,
            "legacy completion without success",
            serde_json::json!({}),
            "completed",
            None,
            turn_id,
            None,
        )
        .await
        .expect("legacy-compatible completion")
        .expect("completed call settled");
    let legacy_event = store
        .protocol_event_store()
        .list_runtime_events(session_id, turn_id)
        .expect("runtime evidence")
        .into_iter()
        .find(|event| {
            matches!(
                &event.msg,
                RuntimeEventMsg::ToolLifecycle { envelope }
                    if envelope.call_id == calls[2]
                        && envelope.status == ToolLifecycleStatus::Completed
            )
        })
        .expect("completed runtime event");
    let mut legacy_message = serde_json::to_value(&legacy_event.msg).expect("runtime JSON");
    legacy_message["envelope"]["success"] = serde_json::Value::Null;
    let legacy_json = serde_json::to_string(&legacy_message).expect("legacy runtime JSON");
    repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute(
            "UPDATE protocol_runtime_events SET msg_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
            params![
                &legacy_json,
                format!("{:x}", Sha256::digest(legacy_json.as_bytes())),
                legacy_event.id.to_string(),
            ],
        )
        .expect("nullable legacy success fixture");

    let active = repository
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Offset {
                offset: 0,
                limit: 1,
            },
            ProtocolPageRequest::Offset {
                offset: 0,
                limit: 1,
            },
        )
        .await
        .expect("older page of active snapshot");
    assert_eq!(active.protocol.turns.items[0].turn_id, older_turn);
    let expected_progress = active.turn_progress.expect("latest turn aggregate");
    assert_eq!(expected_progress.turn_id, turn_id);
    assert_eq!(expected_progress.tool_call_count, 3);
    assert_eq!(expected_progress.completed_tool_count, 1);
    assert_eq!(expected_progress.failed_tool_count, 2);
    assert_eq!(expected_progress.declined_tool_count, 0);
    assert_eq!(expected_progress.cancelled_tool_count, 0);

    let mut terminal = completed_terminal_for_response(session_id, None);
    if let RunEvent::TurnTerminal { terminal, .. } = &mut terminal {
        terminal.tool_call_count = 3;
        terminal.failed_tool_count = 1;
    }
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("latest terminal"),
        AdmittedTerminalCommit::Applied
    );
    let terminal_snapshot = repository
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Latest { limit: 1 },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await
        .expect("bounded terminal snapshot");
    assert!(terminal_snapshot.active_turn_position.is_none());
    assert_eq!(terminal_snapshot.session.status, SessionStatus::Completed);
    assert_eq!(
        terminal_snapshot.turn_progress,
        Some(expected_progress.clone())
    );
    assert_eq!(
        repository
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("durable terminal")
            .expect("terminal exists")
            .failed_tool_count,
        1,
        "display failure counts do not change engine typed failures"
    );

    let sqlite = SqliteStore::open(store.paths()).expect("reopen store");
    sqlite.migrate().expect("migrate reopened store");
    let reopened = StoreBundle::new(sqlite);
    let reloaded = reopened
        .session_repo()
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Offset {
                offset: 0,
                limit: 1,
            },
            ProtocolPageRequest::Offset {
                offset: 0,
                limit: 1,
            },
        )
        .await
        .expect("bounded reopened snapshot");
    assert_eq!(reloaded.protocol.turns.items[0].turn_id, older_turn);
    assert_eq!(reloaded.turn_progress, Some(expected_progress));
}

#[tokio::test]
async fn canonical_turn_progress_omits_incomplete_mixed_native_and_legacy_tool_evidence() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let calls = [ToolCallId::new(), ToolCallId::new()];
    record_progress_shell_calls(&store, session_id, admission_id, turn_id, &calls).await;
    for (call_id, success) in [(calls[0], true), (calls[1], false)] {
        repository
            .complete_tool_call_with_protocol_bundle(
                session_id,
                admission_id,
                call_id,
                crate::tool::ToolName::Shell,
                "operation result",
                serde_json::json!({"success": success}),
                "process result",
                None,
                turn_id,
                None,
            )
            .await
            .expect("canonical completion")
            .expect("call settled");
    }
    let mut terminal = completed_terminal_for_response(session_id, None);
    if let RunEvent::TurnTerminal { terminal, .. } = &mut terminal {
        terminal.tool_call_count = 2;
    }
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("terminal"),
        AdmittedTerminalCommit::Applied
    );
    {
        let mut connection = repository.connection.lock().expect("sqlite mutex");
        let transaction = connection
            .transaction()
            .expect("mixed legacy fixture transaction");
        transaction
            .execute(
                "DELETE FROM protocol_item_append_order
                     WHERE session_id = ?1 AND source_kind = 'runtime_event'
                       AND source_id IN (
                           SELECT id FROM protocol_runtime_events
                           WHERE session_id = ?1 AND turn_id = ?2
                             AND json_extract(msg_json, '$.kind') = 'tool_lifecycle'
                             AND json_extract(msg_json, '$.envelope.call_id') = ?3
                       )",
                params![
                    session_id.to_string(),
                    turn_id.to_string(),
                    calls[1].to_string()
                ],
            )
            .expect("remove legacy call lifecycle append rows");
        transaction
            .execute(
                "DELETE FROM protocol_runtime_events
                     WHERE session_id = ?1 AND turn_id = ?2
                       AND json_extract(msg_json, '$.kind') = 'tool_lifecycle'
                       AND json_extract(msg_json, '$.envelope.call_id') = ?3",
                params![
                    session_id.to_string(),
                    turn_id.to_string(),
                    calls[1].to_string()
                ],
            )
            .expect("retain native lifecycle and legacy canonical evidence");
        transaction.commit().expect("mixed legacy fixture commit");
    }
    let runtime = store
        .protocol_event_store()
        .list_runtime_events(session_id, turn_id)
        .expect("mixed runtime evidence");
    assert!(runtime.iter().any(|event| matches!(
        &event.msg,
        RuntimeEventMsg::ToolLifecycle { envelope }
            if envelope.call_id == calls[0]
                && envelope.status == ToolLifecycleStatus::Completed
                && envelope.success == Some(true)
    )));
    assert!(!runtime.iter().any(|event| matches!(
        &event.msg,
        RuntimeEventMsg::ToolLifecycle { envelope } if envelope.call_id == calls[1]
    )));
    assert_eq!(
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("retained canonical calls")
            .iter()
            .filter(|item| matches!(&item.payload, HistoryItemPayload::ToolCall { .. }))
            .count(),
        2
    );
    let snapshot = repository
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Latest { limit: 1 },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await
        .expect("bounded mixed legacy snapshot");
    assert!(matches!(
        &snapshot.protocol.history.items[0].payload,
        HistoryItemPayload::ToolOutput { call_id, success: Some(false), .. }
            if *call_id == calls[1]
    ));
    assert!(
        snapshot.turn_progress.is_none(),
        "partial runtime coverage must not hide the legacy call's unsuccessful operation"
    );
}

#[tokio::test]
async fn canonical_turn_page_reprojects_legacy_completed_non_success_tool_status() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let call_id = ToolCallId::new();
    record_progress_shell_calls(&store, session_id, admission_id, turn_id, &[call_id]).await;
    repository
        .complete_tool_call_with_protocol_bundle(
            session_id,
            admission_id,
            call_id,
            crate::tool::ToolName::Shell,
            "unsuccessful operation",
            serde_json::json!({"success": false, "exit_code": 7}),
            "Exit code: 7",
            None,
            turn_id,
            None,
        )
        .await
        .expect("canonical completion")
        .expect("call settled");

    let native = repository
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Offset {
                offset: 0,
                limit: 1,
            },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await
        .expect("current projector snapshot");
    let current_statuses =
        crate::tui::state::tool_statuses_from_turn_items(&native.protocol.turns.items);
    assert_eq!(current_statuses.len(), 1);
    assert_eq!(
        current_statuses[0].status,
        crate::session::ToolCallStatus::Failed
    );
    let mut legacy_item = native.protocol.turns.items[0].clone();
    let source_id = legacy_item.source_item_id.expect("exact history source");
    let source = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history evidence")
        .into_iter()
        .find(|item| item.id == source_id)
        .expect("completion source");
    assert_eq!(source.session_id, session_id);
    assert!(matches!(source.scope, HistoryScope::Turn { turn_id: owner } if owner == turn_id));
    assert_eq!(source.sequence_no, legacy_item.sequence_no);
    assert!(matches!(
        &source.payload,
        HistoryItemPayload::ToolOutput {
            call_id: owner,
            status: ToolLifecycleStatus::Completed,
            success: Some(false),
            ..
        } if *owner == call_id
    ));
    let runtime = store
        .protocol_event_store()
        .list_runtime_events(session_id, turn_id)
        .expect("runtime evidence")
        .into_iter()
        .find(|event| event.sequence_no == legacy_item.sequence_no)
        .expect("same-sequence completion runtime");
    assert!(matches!(
        &runtime.msg,
        RuntimeEventMsg::ToolLifecycle { envelope }
            if envelope.call_id == call_id
                && envelope.status == ToolLifecycleStatus::Completed
                && envelope.success == Some(false)
    ));

    let TurnItemPayload::ToolStatus { status, .. } = &mut legacy_item.payload else {
        panic!("completion display item");
    };
    *status = ToolLifecycleStatus::Completed;
    let legacy_json = serde_json::to_string(&legacy_item.payload).expect("legacy display JSON");
    let legacy_hash = format!("{:x}", Sha256::digest(legacy_json.as_bytes()));
    let source_json = serde_json::to_string(&source.payload).expect("source JSON");
    let source_hash = format!("{:x}", Sha256::digest(source_json.as_bytes()));
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        assert_eq!(
                connection
                    .execute(
                        "UPDATE protocol_turn_items SET payload_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
                        params![&legacy_json, &legacy_hash, legacy_item.id.to_string()],
                    )
                    .expect("old projector display fixture"),
                1
            );
    }

    let sqlite = SqliteStore::open(store.paths()).expect("reopen old display record");
    sqlite.migrate().expect("migrate reopened store");
    let reopened = StoreBundle::new(sqlite);
    let replay = reopened
        .session_repo()
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Offset {
                offset: 0,
                limit: 1,
            },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await
        .expect("bounded legacy display snapshot");
    assert!(
        !replay
            .protocol
            .history
            .items
            .iter()
            .any(|item| item.id == source_id)
    );
    let progress = replay.turn_progress.expect("complete runtime aggregate");
    assert_eq!(progress.tool_call_count, 1);
    assert_eq!(progress.completed_tool_count, 0);
    assert_eq!(progress.failed_tool_count, 1);
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        let persisted_display: (String, String) = connection
            .query_row(
                "SELECT payload_json, payload_sha256 FROM protocol_turn_items WHERE id = ?1",
                params![legacy_item.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("persisted legacy display");
        assert_eq!(persisted_display, (legacy_json, legacy_hash));
        let persisted_source: (String, String) = connection
            .query_row(
                "SELECT payload_json, payload_sha256 FROM protocol_history_items WHERE id = ?1",
                params![source_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("persisted completion source");
        assert_eq!(persisted_source, (source_json, source_hash));
    }
    let statuses = crate::tui::state::tool_statuses_from_turn_items(&replay.protocol.turns.items);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].tool_call_id, call_id);
    assert_eq!(
        statuses[0].status,
        crate::session::ToolCallStatus::Failed,
        "legacy display status must agree with its exact unsuccessful operation and runtime aggregate"
    );
}

#[tokio::test]
async fn canonical_turn_page_keeps_legacy_completion_without_explicit_source_success() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let call_id = ToolCallId::new();
    record_progress_shell_calls(&store, session_id, admission_id, turn_id, &[call_id]).await;
    repository
        .complete_tool_call_with_protocol_bundle(
            session_id,
            admission_id,
            call_id,
            crate::tool::ToolName::Shell,
            "legacy operation",
            serde_json::json!({}),
            "completed without operation outcome evidence",
            None,
            turn_id,
            None,
        )
        .await
        .expect("canonical completion")
        .expect("call settled");
    let mut display = store
        .protocol_event_store()
        .list_turn_items(session_id, turn_id)
        .expect("display evidence")
        .into_iter()
        .find(|item| {
            matches!(
                &item.payload,
                TurnItemPayload::ToolStatus {
                    call_id: owner,
                    status: ToolLifecycleStatus::Completed,
                    ..
                } if *owner == call_id
            )
        })
        .expect("completion display");
    let source_id = display.source_item_id.expect("source identity");
    let mut source = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history evidence")
        .into_iter()
        .find(|item| item.id == source_id)
        .expect("completion source");
    let TurnItemPayload::ToolStatus { status, .. } = &mut display.payload else {
        panic!("tool display");
    };
    *status = ToolLifecycleStatus::Completed;
    let HistoryItemPayload::ToolOutput { success, .. } = &mut source.payload else {
        panic!("tool output source");
    };
    *success = None;
    let mut runtime = store
        .protocol_event_store()
        .list_runtime_events(session_id, turn_id)
        .expect("runtime evidence")
        .into_iter()
        .find(|event| event.sequence_no == display.sequence_no)
        .expect("same-sequence completion runtime");
    let RuntimeEventMsg::ToolLifecycle { envelope } = &mut runtime.msg else {
        panic!("tool completion runtime");
    };
    assert_eq!(envelope.call_id, call_id);
    assert_eq!(envelope.status, ToolLifecycleStatus::Completed);
    envelope.success = None;
    let display_json = serde_json::to_string(&display.payload).expect("old display JSON");
    let source_json = serde_json::to_string(&source.payload).expect("old source JSON");
    let runtime_json = serde_json::to_string(&runtime.msg).expect("old runtime JSON");
    {
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
                .execute(
                    "UPDATE protocol_turn_items SET payload_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
                    params![
                        &display_json,
                        format!("{:x}", Sha256::digest(display_json.as_bytes())),
                        display.id.to_string(),
                    ],
                )
                .expect("old completion display fixture");
        connection
                .execute(
                    "UPDATE protocol_history_items SET payload_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
                    params![
                        &source_json,
                        format!("{:x}", Sha256::digest(source_json.as_bytes())),
                        source_id.to_string(),
                    ],
                )
                .expect("legacy source without explicit success fixture");
        connection
                .execute(
                    "UPDATE protocol_runtime_events SET msg_json = ?1, payload_sha256 = ?2 WHERE id = ?3",
                    params![
                        &runtime_json,
                        format!("{:x}", Sha256::digest(runtime_json.as_bytes())),
                        runtime.id.to_string(),
                    ],
                )
                .expect("matching legacy runtime without explicit success fixture");
    }
    let snapshot = repository
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Latest { limit: 1 },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await
        .expect("legacy snapshot without explicit operation outcome");
    assert!(matches!(
        &snapshot.protocol.history.items[0].payload,
        HistoryItemPayload::ToolOutput { success: None, metadata, .. }
            if metadata.as_object().is_some_and(|object| object.is_empty())
    ));
    let progress = snapshot.turn_progress.expect("legacy runtime aggregate");
    assert_eq!(progress.completed_tool_count, 1);
    assert_eq!(progress.failed_tool_count, 0);
    let statuses = crate::tui::state::tool_statuses_from_turn_items(&snapshot.protocol.turns.items);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].tool_call_id, call_id);
    assert_eq!(
        statuses[0].status,
        crate::session::ToolCallStatus::Completed,
        "legacy completion without explicit outcome evidence remains completed"
    );
}

#[tokio::test]
async fn canonical_turn_progress_preserves_legacy_tool_evidence_without_runtime_lifecycle() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let call_id = ToolCallId::new();
    record_progress_shell_calls(&store, session_id, admission_id, turn_id, &[call_id]).await;
    repository
        .complete_tool_call_with_protocol_bundle(
            session_id,
            admission_id,
            call_id,
            crate::tool::ToolName::Shell,
            "unsuccessful operation",
            serde_json::json!({"success": false}),
            "Exit code: 7",
            None,
            turn_id,
            None,
        )
        .await
        .expect("canonical completion")
        .expect("call settled");
    let mut terminal = completed_terminal_for_response(session_id, None);
    if let RunEvent::TurnTerminal { terminal, .. } = &mut terminal {
        terminal.tool_call_count = 1;
    }
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("terminal"),
        AdmittedTerminalCommit::Applied
    );
    {
        let mut connection = repository.connection.lock().expect("sqlite mutex");
        let transaction = connection
            .transaction()
            .expect("legacy fixture transaction");
        transaction
            .execute(
                "DELETE FROM protocol_item_append_order
                     WHERE session_id = ?1 AND source_kind = 'runtime_event'
                       AND source_id IN (
                           SELECT id FROM protocol_runtime_events
                           WHERE session_id = ?1 AND turn_id = ?2
                             AND json_extract(msg_json, '$.kind') = 'tool_lifecycle'
                       )",
                params![session_id.to_string(), turn_id.to_string()],
            )
            .expect("remove lifecycle append rows");
        transaction
            .execute(
                "DELETE FROM protocol_runtime_events
                     WHERE session_id = ?1 AND turn_id = ?2
                       AND json_extract(msg_json, '$.kind') = 'tool_lifecycle'",
                params![session_id.to_string(), turn_id.to_string()],
            )
            .expect("legacy tool evidence without runtime lifecycle");
        transaction.commit().expect("legacy fixture commit");
    }
    let snapshot = repository
        .canonical_session_protocol_snapshot(
            session_id,
            ProtocolPageRequest::Latest { limit: 1 },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await
        .expect("bounded legacy snapshot");
    assert!(snapshot.active_turn_position.is_none());
    assert_eq!(
        snapshot.protocol.latest_turn_position.map(|(id, _)| id),
        Some(turn_id)
    );
    assert!(matches!(
        &snapshot.protocol.history.items[0].payload,
        HistoryItemPayload::ToolOutput { call_id: stored_call, success: Some(false), .. }
            if *stored_call == call_id
    ));
    assert!(
            store
                .protocol_event_store()
                .list_turn_items(session_id, turn_id)
                .expect("retained canonical display evidence")
                .iter()
                .any(|item| matches!(
                    &item.payload,
                    TurnItemPayload::ToolStatus { call_id: stored_call, status: ToolLifecycleStatus::Failed, .. }
                        if *stored_call == call_id
                ))
        );
    assert!(
        snapshot.turn_progress.is_none(),
        "missing runtime lifecycle evidence must not replace historical display counts with zero"
    );
}

#[tokio::test]
async fn pending_tool_sidecar_and_canonical_history_are_one_atomic_bundle() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let repo = store.session_repo();
    repo.connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch(
            "CREATE TRIGGER abort_tool_sidecar
                 BEFORE INSERT ON tool_calls
                 BEGIN SELECT RAISE(ABORT, 'injected sidecar failure'); END;",
        )
        .expect("trigger");
    let result = repo
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id: ModelResponseId::new(),
                assistant_text: Some("I will run the command.".to_string()),
                assistant_protocol_sequence_no: None,
                tool_calls: vec![PendingToolCallWrite {
                    id: ToolCallId::new(),
                    model_call_id: "model-call-1".to_string(),
                    tool_name: "shell".to_string(),
                    arguments_json: serde_json::json!({"command": "echo ok"}).to_string(),
                    protocol_sequence_no: None,
                }],
            },
        )
        .await;
    assert!(result.is_err());
    let history = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history");
    assert_eq!(
        history
            .iter()
            .filter(|item| {
                matches!(
                    item.payload,
                    HistoryItemPayload::AssistantMessage { .. }
                        | HistoryItemPayload::ToolCall { .. }
                )
            })
            .count(),
        0,
        "failed sidecar insert must roll back the complete model response bundle"
    );
    assert_eq!(
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("runtime events")
            .iter()
            .filter(|event| matches!(event.msg, RuntimeEventMsg::ToolLifecycle { .. }))
            .count(),
        0,
        "failed sidecar insert must roll back its runtime projection"
    );
}

#[tokio::test]
async fn pending_tool_call_preserves_unknown_name_and_invalid_provider_json_verbatim() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let response_id = ModelResponseId::new();
    let call_id = ToolCallId::new();
    let raw_tool_name = "provider_tool_not_in_router".to_string();
    let raw_arguments_json = "{not-json}".to_string();
    let events = store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id,
                assistant_text: None,
                assistant_protocol_sequence_no: None,
                tool_calls: vec![PendingToolCallWrite {
                    id: call_id,
                    model_call_id: "provider-call-raw".to_string(),
                    tool_name: raw_tool_name.clone(),
                    arguments_json: raw_arguments_json.clone(),
                    protocol_sequence_no: None,
                }],
            },
        )
        .await
        .expect("raw pending tool call");

    assert!(matches!(
        events.as_slice(),
        [RunEvent::ToolCallPending {
            tool_call_id: stored_call_id,
            response_id: stored_response_id,
            model_call_id,
            tool_name,
            arguments_json,
        }] if *stored_call_id == call_id
            && *stored_response_id == response_id
            && model_call_id == "provider-call-raw"
            && tool_name == &raw_tool_name
            && arguments_json == &raw_arguments_json
    ));
    let history = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("canonical raw history");
    assert!(history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall {
            call_id: stored_call_id,
            response_id: stored_response_id,
            model_call_id,
            tool_name,
            arguments_json,
        } if *stored_call_id == call_id
            && *stored_response_id == response_id
            && model_call_id == "provider-call-raw"
            && tool_name == &raw_tool_name
            && arguments_json == &raw_arguments_json
    )));
    let sidecar = store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT status, history_item_id FROM tool_calls WHERE id = ?1",
            [call_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("minimal pending sidecar");
    assert_eq!(sidecar.0, "pending");
    assert!(history.iter().any(|item| item.id.to_string() == sidecar.1));
}

#[tokio::test]
async fn complete_model_response_bundle_commits_all_calls_before_execution() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let response_id = ModelResponseId::new();
    let first_call_id = ToolCallId::new();
    let second_call_id = ToolCallId::new();
    let events = store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id,
                assistant_text: Some("I will inspect both inputs.".to_string()),
                assistant_protocol_sequence_no: Some(0),
                tool_calls: vec![
                    PendingToolCallWrite {
                        id: first_call_id,
                        model_call_id: "provider-call-a".to_string(),
                        tool_name: "read".to_string(),
                        arguments_json: serde_json::json!({"path": "a.txt"}).to_string(),
                        protocol_sequence_no: Some(1),
                    },
                    PendingToolCallWrite {
                        id: second_call_id,
                        model_call_id: "provider-call-b".to_string(),
                        tool_name: "read".to_string(),
                        arguments_json: serde_json::json!({"path": "b.txt"}).to_string(),
                        protocol_sequence_no: Some(2),
                    },
                ],
            },
        )
        .await
        .expect("model response bundle");
    assert_eq!(events.len(), 3);

    let history = store
        .protocol_event_store()
        .list_history_items(session_id, turn_id)
        .expect("history");
    let response_history = history
        .iter()
        .filter(|item| {
            matches!(
                item.payload,
                HistoryItemPayload::AssistantMessage { .. } | HistoryItemPayload::ToolCall { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        response_history.as_slice(),
        [
            HistoryItem {
                payload: HistoryItemPayload::AssistantMessage { response_id: stored, .. },
                ..
            },
            HistoryItem {
                payload: HistoryItemPayload::ToolCall { call_id: first, response_id: first_response, .. },
                ..
            },
            HistoryItem {
                payload: HistoryItemPayload::ToolCall { call_id: second, response_id: second_response, .. },
                ..
            }
        ] if *stored == response_id
            && *first == first_call_id
            && *second == second_call_id
            && *first_response == response_id
            && *second_response == response_id
    ));
    let sidecar_count = store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row("SELECT COUNT(*) FROM tool_calls", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("sidecar count");
    assert_eq!(sidecar_count, 2);
}

#[tokio::test]
async fn rollback_is_one_transaction_and_preserves_session_scoped_mode() {
    let (store, session_id) = test_repo().await;
    let protocol = store.protocol_event_store();
    protocol
        .set_collaboration_mode(session_id, ModeKind::Plan)
        .expect("store plan mode")
        .expect("plan instruction");
    let (admission_id, real_turn) = active_turn(&store, session_id).await;
    protocol
        .set_collaboration_mode(session_id, ModeKind::Default)
        .expect("store default mode")
        .expect("default instruction");
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &completed_terminal(session_id),
                real_turn,
                None,
                None,
            )
            .await
            .expect("terminal"),
        AdmittedTerminalCommit::Applied
    );
    let revision_before_rollback = store
        .session_repo()
        .active_turn_expectation_for_session(session_id)
        .await
        .expect("expectation before rollback")
        .expect("session exists")
        .revision();
    assert_eq!(revision_before_rollback, 1);

    store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch(
            "CREATE TRIGGER abort_session_rollback
                 BEFORE UPDATE OF status ON sessions
                 BEGIN SELECT RAISE(ABORT, 'injected rollback reset failure'); END;",
        )
        .expect("rollback failure trigger");
    assert!(
        store
            .session_repo()
            .rollback_session_transaction(session_id, 1)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .session_repo()
            .active_turn_expectation_for_session(session_id)
            .await
            .expect("expectation after aborted rollback")
            .expect("session exists")
            .revision(),
        revision_before_rollback,
        "the allocator delete and revision bump must roll back atomically"
    );
    assert_eq!(
        protocol
            .list_history_items_for_session(session_id)
            .expect("history after failed rollback")
            .len(),
        3,
        "a reset failure must roll turn deletion back while retaining session state"
    );
    store
        .session_repo()
        .connection
        .lock()
        .expect("sqlite mutex")
        .execute_batch("DROP TRIGGER abort_session_rollback;")
        .expect("drop rollback failure trigger");

    let result = store
        .session_repo()
        .rollback_session_transaction(session_id, 1)
        .await
        .expect("rollback latest turn");
    assert_eq!(result.dropped_turn_ids, vec![real_turn]);
    assert_eq!(result.remaining_history_items, 2);
    assert_eq!(result.session.status, SessionStatus::Idle);
    assert_eq!(
        store
            .session_repo()
            .active_turn_expectation_for_session(session_id)
            .await
            .expect("expectation after rollback")
            .expect("session exists")
            .revision(),
        revision_before_rollback + 1,
    );
    assert_eq!(
        protocol
            .collaboration_mode_for_session(session_id)
            .expect("mode after rollback"),
        ModeKind::Default
    );
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    for table in [
        "protocol_runtime_events",
        "protocol_history_items",
        "protocol_turn_items",
        "protocol_item_append_order",
        "protocol_turn_sequence_allocators",
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1 AND turn_id = ?2");
        let count = connection
            .query_row(
                &sql,
                params![session_id.to_string(), real_turn.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("rolled-back table count");
        assert_eq!(count, 0, "{table} retained rolled-back turn state");
    }
}

#[tokio::test]
async fn rollback_advances_admission_revision_and_rejects_stale_idle_a_after_b() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let turn_a = TurnId::new();
    let admitted_a = repository
        .admit_session_turn(session_id, turn_a)
        .await
        .expect("admit A")
        .expect("A owner");
    assert_eq!(admitted_a.admission_revision, 1);
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admitted_a.admission_id,
                &completed_terminal(session_id),
                turn_a,
                None,
                None,
            )
            .await
            .expect("terminalize A"),
        AdmittedTerminalCommit::Applied
    );
    let stale_idle_a = ActiveTurnExpectation::Idle {
        latest_turn_id: Some(turn_a),
        revision: admitted_a.admission_revision,
    };

    let turn_b = TurnId::new();
    let admitted_b = repository
        .admit_session_turn(session_id, turn_b)
        .await
        .expect("admit B")
        .expect("B owner");
    assert_eq!(admitted_b.admission_revision, 2);
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admitted_b.admission_id,
                &completed_terminal(session_id),
                turn_b,
                None,
                None,
            )
            .await
            .expect("terminalize B"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        repository
            .active_turn_expectation_for_session(session_id)
            .await
            .expect("terminal B expectation")
            .expect("session exists")
            .revision(),
        admitted_b.admission_revision,
        "same-turn terminal appends must not allocate a new admission revision"
    );

    let rolled_back = repository
        .rollback_session_transaction(session_id, 1)
        .await
        .expect("rollback B");
    assert_eq!(rolled_back.dropped_turn_ids, vec![turn_b]);
    let current = repository
        .active_turn_expectation_for_session(session_id)
        .await
        .expect("current expectation")
        .expect("session exists");
    assert_eq!(
        current,
        ActiveTurnExpectation::Idle {
            latest_turn_id: Some(turn_a),
            revision: 3,
        }
    );
    assert_ne!(current, stale_idle_a);

    assert!(
        repository
            .admit_session_turn_with_initial_user_turn_after_latest(
                session_id,
                TurnId::new(),
                None,
                Some(turn_a),
                admitted_a.admission_revision,
            )
            .await
            .expect("stale Idle(A) admission decision")
            .is_none(),
        "rollback must not resurrect the pre-B Idle(A) admission capability"
    );
}

#[tokio::test]
async fn rollback_removes_only_latest_turn_harness_lineage_and_reopens_cleanly() {
    let (store, session_id) = test_repo().await;
    let sibling = create_sibling_session(&store, session_id, "sibling").await;
    let paths = store.paths().clone();
    let run_store = store.harness_run_store();

    let (earlier_admission, earlier_turn) = active_turn(&store, session_id).await;
    let earlier_run = HarnessRunId::new();
    run_store
        .upsert_run(&started_harness_run(earlier_run, session_id, earlier_turn))
        .expect("earlier harness run");
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                earlier_admission,
                &completed_terminal(session_id),
                earlier_turn,
                None,
                None,
            )
            .await
            .expect("earlier terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        run_store
            .project_canonical_terminal_for_turn(session_id, earlier_turn)
            .expect("earlier harness terminal"),
        1
    );

    let (latest_admission, latest_turn) = active_turn(&store, session_id).await;
    let latest_run = HarnessRunId::new();
    run_store
        .upsert_run(&started_harness_run(latest_run, session_id, latest_turn))
        .expect("latest harness run");
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                latest_admission,
                &completed_terminal(session_id),
                latest_turn,
                None,
                None,
            )
            .await
            .expect("latest terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        run_store
            .project_canonical_terminal_for_turn(session_id, latest_turn)
            .expect("latest harness terminal"),
        1
    );
    let latest_started_run = HarnessRunId::new();
    run_store
        .upsert_run(&started_harness_run(
            latest_started_run,
            session_id,
            latest_turn,
        ))
        .expect("late-started harness run for latest turn");

    let (sibling_admission, sibling_turn) = active_turn(&store, sibling.id).await;
    let sibling_run = HarnessRunId::new();
    run_store
        .upsert_run(&started_harness_run(sibling_run, sibling.id, sibling_turn))
        .expect("sibling harness run");
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                sibling.id,
                sibling_admission,
                &completed_terminal(sibling.id),
                sibling_turn,
                None,
                None,
            )
            .await
            .expect("sibling terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        run_store
            .project_canonical_terminal_for_turn(sibling.id, sibling_turn)
            .expect("sibling harness terminal"),
        1
    );

    let unmapped_run = HarnessRunId::new();
    run_store
        .upsert_run(&HarnessRunRecord {
            id: unmapped_run,
            session_id: Some(session_id),
            protocol_turn_id: None,
            canonical_terminal_runtime_event_id: None,
            workspace_root: "C:/workspace".into(),
            artifact_root: Utf8PathBuf::from(format!("C:/artifacts/{unmapped_run}")),
            mode: "legacy_replay".to_string(),
            started_at_ms: 5,
            completed_at_ms: Some(6),
            status: HarnessRunStatus::Pass,
        })
        .expect("legacy unmapped harness run");

    {
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        connection
            .execute(
                "INSERT INTO harness_artifacts
                     (id, run_id, kind, relative_path, sha256, size_bytes,
                      tags_json, created_by_event_id, contract_refs_json,
                      created_at_ms)
                     VALUES ('latest-artifact', ?1, '\"verification_log\"',
                             'latest.json', 'artifact-hash', 1, '[]', NULL,
                             '[]', 11)",
                [latest_run.to_string()],
            )
            .expect("latest artifact");
        connection
            .execute(
                "INSERT INTO harness_contracts
                     (run_id, contract_id, kind, version, source_path,
                      content_sha256, schema_ref, model_visible_summary)
                     VALUES (?1, 'latest-contract', 'test', '1', 'contract.md',
                             'contract-hash', NULL, NULL)",
                [latest_run.to_string()],
            )
            .expect("latest contract");
        connection
            .execute(
                "INSERT INTO harness_gate_results
                     (id, run_id, sequence_no, gate_kind, status, severity,
                      owner, summary, payload_json, created_at_ms)
                     VALUES ('latest-gate', ?1, 1, 'test', 'pass', 'info',
                             NULL, 'pass', '{}', 11)",
                [latest_run.to_string()],
            )
            .expect("latest gate");
        connection
            .execute(
                "INSERT INTO harness_replay_reports
                     (run_id, schema_version, status, primary_owner, summary,
                      restart_point, next_actions_json, report_json,
                      created_at_ms)
                     VALUES (?1, '1', 'pass', NULL, 'pass', NULL, '[]', '{}', 11)",
                [latest_run.to_string()],
            )
            .expect("latest replay report");
        connection
            .execute(
                "INSERT INTO harness_events
                     (id, run_id, sequence_no, kind, payload_json,
                      contract_refs_json, artifact_refs_json, parent_event_id,
                      payload_sha256, created_at_ms)
                     VALUES ('latest-started-event', ?1, 0,
                             '\"state_snapshot_recorded\"',
                             '{\"type\":\"generic\",\"data\":null}', '[]', '[]',
                             NULL, 'snapshot-hash', 11)",
                [latest_started_run.to_string()],
            )
            .expect("latest started harness event");
        connection
            .execute_batch(
                "CREATE TRIGGER abort_harness_linked_session_rollback
                     BEFORE UPDATE OF status ON sessions
                     BEGIN SELECT RAISE(ABORT, 'injected rollback reset failure'); END;",
            )
            .expect("rollback failure trigger");
    }

    assert!(
        store
            .session_repo()
            .rollback_session_transaction(session_id, 1)
            .await
            .is_err()
    );
    assert!(
        run_store
            .get_run(latest_run)
            .expect("latest run after failed rollback")
            .is_some(),
        "a later rollback failure must restore harness lineage"
    );
    assert!(
        run_store
            .get_run(latest_started_run)
            .expect("latest started run after failed rollback")
            .is_some(),
        "a later rollback failure must restore every mapped harness run"
    );
    {
        let repository = store.session_repo();
        let connection = repository.connection.lock().expect("sqlite mutex");
        for table in [
            "harness_events",
            "harness_artifacts",
            "harness_contracts",
            "harness_gate_results",
            "harness_replay_reports",
        ] {
            let count = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE run_id = ?1"),
                    [latest_run.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("latest harness child after failed rollback");
            assert!(count > 0, "{table} was not transactionally restored");
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM harness_events WHERE run_id = ?1",
                    [latest_started_run.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("started harness event after failed rollback"),
            1,
            "a later rollback failure must restore children of every mapped run"
        );
        connection
            .execute_batch("DROP TRIGGER abort_harness_linked_session_rollback;")
            .expect("drop rollback failure trigger");
    }

    let result = store
        .session_repo()
        .rollback_session_transaction(session_id, 1)
        .await
        .expect("rollback latest harness-linked turn");
    assert_eq!(result.dropped_turn_ids, vec![latest_turn]);
    assert!(
        run_store
            .get_run(latest_run)
            .expect("latest run after rollback")
            .is_none()
    );
    assert!(
        run_store
            .get_run(latest_started_run)
            .expect("latest started run after rollback")
            .is_none()
    );
    assert!(
        run_store
            .get_run(earlier_run)
            .expect("earlier run after rollback")
            .is_some()
    );
    assert!(
        run_store
            .get_run(sibling_run)
            .expect("sibling run after rollback")
            .is_some()
    );
    assert!(
        run_store
            .get_run(unmapped_run)
            .expect("unmapped run after rollback")
            .is_some()
    );
    assert_eq!(
        store
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("foreign-key check after rollback"),
        0
    );

    let reopened = SqliteStore::open(&paths).expect("reopen store after rollback");
    reopened
        .migrate()
        .expect("reopened storage passes current V52 linkage validation");
    let reopened_run_store = reopened.harness_run_store();
    assert!(
        reopened_run_store
            .get_run(latest_run)
            .expect("reopened latest run")
            .is_none()
    );
    assert!(
        reopened_run_store
            .get_run(earlier_run)
            .expect("reopened earlier run")
            .is_some()
    );
    assert!(
        reopened_run_store
            .get_run(sibling_run)
            .expect("reopened sibling run")
            .is_some()
    );
    assert!(
        reopened_run_store
            .get_run(unmapped_run)
            .expect("reopened unmapped run")
            .is_some()
    );
    assert_eq!(
        reopened
            .session_repo()
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("foreign-key check after reopen"),
        0
    );
}

#[tokio::test]
async fn rollback_rejects_an_active_admission_anywhere_in_the_root_tree() {
    let (store, root_session_id) = test_repo().await;
    let root = store
        .session_repo()
        .get_session(root_session_id)
        .await
        .expect("root session");
    let child = store
        .session_repo()
        .create_session(NewSession {
            project_id: root.project_id,
            title: "child".to_string(),
            cwd: root.cwd.clone(),
            model: root.model.clone(),
            base_url: root.base_url.clone(),
            access_mode: root.access_mode,
            provider_connection: root.provider_connection.clone(),
        })
        .await
        .expect("child session");
    store
        .session_repo()
        .insert_session_spawn_edge(
            root_session_id,
            root_session_id,
            child.id,
            "/root/child",
            "child",
        )
        .await
        .expect("spawn edge");
    store
        .protocol_event_store()
        .set_collaboration_mode(root_session_id, ModeKind::Plan)
        .expect("root history")
        .expect("root mode item");
    store
        .session_repo()
        .admit_session_turn(child.id, TurnId::new())
        .await
        .expect("child admission")
        .expect("child admitted");

    let error = store
        .session_repo()
        .rollback_session_transaction(root_session_id, 1)
        .await
        .expect_err("active child must block root rollback");
    assert!(error.to_string().contains(&child.id.to_string()));
    assert_eq!(
        store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("retained root history")
            .len(),
        1
    );
}

#[tokio::test]
async fn active_fork_settles_unfinished_calls_before_its_interrupted_terminal() {
    let (store, source_session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, source_session_id).await;
    let response_id = ModelResponseId::new();
    let call_id = ToolCallId::new();
    store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            source_session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id,
                assistant_text: Some("I will inspect the file.".to_string()),
                assistant_protocol_sequence_no: None,
                tool_calls: vec![PendingToolCallWrite {
                    id: call_id,
                    model_call_id: "provider-call".to_string(),
                    tool_name: "read".to_string(),
                    arguments_json: serde_json::json!({"path": "README.md"}).to_string(),
                    protocol_sequence_no: None,
                }],
            },
        )
        .await
        .expect("pending response");

    let fork = store
        .session_repo()
        .fork_session_snapshot(source_session_id, Some("snapshot".to_string()))
        .await
        .expect("active snapshot fork");
    assert!(fork.interrupted_live_snapshot);
    let forked_history = store
        .protocol_event_store()
        .list_history_items(fork.forked_session.id, turn_id)
        .expect("forked history");
    assert!(forked_history.iter().any(|item| matches!(
        item.payload,
        HistoryItemPayload::ToolOutput {
            call_id: stored_call_id,
            status: ToolLifecycleStatus::Cancelled,
            ..
        } if stored_call_id == call_id
    )));
    let terminal = store
        .session_repo()
        .durable_terminal_for_turn(fork.forked_session.id, turn_id)
        .await
        .expect("fork terminal read")
        .expect("fork terminal");
    assert!(matches!(
        terminal.outcome,
        TurnTerminalOutcome::Interrupted { .. }
    ));
    assert_eq!(terminal.final_response_id, None);
    assert_eq!(terminal.tool_call_count, 1);
    assert_eq!(terminal.failed_tool_count, 0);
    assert_eq!(terminal.change_count, 0);
    let forked_turn_items = store
        .protocol_event_store()
        .list_turn_items(fork.forked_session.id, turn_id)
        .expect("forked turn items");
    let cancelled_position = forked_turn_items
        .iter()
        .position(|item| {
            matches!(
                item.payload,
                TurnItemPayload::ToolStatus {
                    call_id: stored_call_id,
                    status: ToolLifecycleStatus::Cancelled,
                    ..
                } if stored_call_id == call_id
            )
        })
        .expect("cancelled projection");
    let terminal_position = forked_turn_items
        .iter()
        .position(|item| matches!(item.payload, TurnItemPayload::Terminal { .. }))
        .expect("terminal projection");
    assert!(cancelled_position < terminal_position);
}

#[tokio::test]
async fn active_fork_rejects_a_source_without_an_active_turn() {
    let (store, source_session_id) = test_repo().await;
    store
        .session_repo()
        .inject_raw_runtime_state_for_corruption_test(
            source_session_id,
            "running",
            None,
            None,
            None,
        )
        .expect("create impossible running source fixture");

    let error = store
        .session_repo()
        .fork_session_snapshot(source_session_id, Some("invalid snapshot".to_string()))
        .await
        .expect_err("fork must fail closed without an active turn");

    assert!(error.to_string().contains("durable run admission"));
}

#[tokio::test]
async fn edit_fork_retains_canonical_image_bytes_without_source_file() {
    use base64::Engine as _;
    let (store, source) = test_repo().await;
    let repository = store.session_repo();
    let turn_id = TurnId::new();
    let admission = repository
        .admit_session_turn(source, turn_id)
        .await
        .unwrap()
        .unwrap()
        .admission_id;
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend(1_u32.to_be_bytes());
    bytes.extend(1_u32.to_be_bytes());
    let image = crate::session::ImagePart {
        source_path: Some("missing-original.png".into()),
        mime_type: "image/png".into(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        byte_len: bytes.len() as u64,
    };
    repository
        .append_user_turn_with_protocol_bundle(
            source,
            admission,
            &UserTurn {
                turn_id,
                items: vec![
                    UserInputItem::Text {
                        text: "edit with this image".into(),
                    },
                    UserInputItem::Image {
                        image: image.clone(),
                    },
                ],
                prompt_dispatch: None,
                editor_context: None,
            },
            turn_id,
            0,
        )
        .await
        .unwrap();
    repository
        .terminalize_admitted_turn_with_protocol_event(
            source,
            admission,
            &completed_terminal(source),
            turn_id,
            None,
            None,
        )
        .await
        .unwrap();
    let revision = repository
        .active_turn_expectation_for_session(source)
        .await
        .unwrap()
        .unwrap()
        .revision();
    let revisions = repository
        .origin_history_revisions_for_session_tree(source, 2)
        .unwrap();
    let fork = repository
        .fork_latest_turn_for_edit_checked(source, turn_id, revision, &revisions)
        .await
        .unwrap();
    assert_eq!(fork.editable_text, "edit with this image");
    assert_eq!(fork.retained_images.len(), 1);
    assert_eq!(fork.retained_images[0].data_base64, image.data_base64);
    assert_eq!(fork.retained_images[0].source_path, image.source_path);
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(fork.forked_session.id)
            .unwrap()
            .iter()
            .all(|item| item.turn_id() != Some(turn_id))
    );
    let source_history = store
        .protocol_event_store()
        .list_history_items_for_session(source)
        .unwrap();
    assert!(source_history.iter().any(|item| matches!(&item.payload, HistoryItemPayload::UserTurn { content, .. } if content.iter().any(|part| matches!(part, ContentPart::Image { image: saved } if saved.data_base64 == image.data_base64)))));
}

#[tokio::test]
async fn edit_fork_keeps_stopped_turn_only_in_source_and_checks_exact_idle_target() {
    let (store, source_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (first_admission, first_turn) = active_turn(&store, source_session_id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                source_session_id,
                first_admission,
                &completed_terminal(source_session_id),
                first_turn,
                None,
                None,
            )
            .await
            .expect("first terminal"),
        AdmittedTerminalCommit::Applied
    );
    let (stopped_admission, stopped_turn) = active_turn(&store, source_session_id).await;
    record_text_response(
        &store,
        source_session_id,
        stopped_admission,
        stopped_turn,
        "answer from the stopped turn",
    )
    .await;
    let stop = RunEvent::TurnTerminal {
        session_id: source_session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                source_session_id,
                stopped_admission,
                &stop,
                stopped_turn,
                None,
                None,
            )
            .await
            .expect("stop terminal"),
        AdmittedTerminalCommit::Applied
    );
    let expected_revision = repository
        .active_turn_expectation_for_session(source_session_id)
        .await
        .expect("idle state")
        .expect("source exists")
        .revision();
    let expected_revisions = repository
        .origin_history_revisions_for_session_tree(source_session_id, 1)
        .expect("history revision");
    assert!(
        repository
            .fork_latest_turn_for_edit_checked(
                source_session_id,
                first_turn,
                expected_revision,
                &expected_revisions,
            )
            .await
            .is_err()
    );
    assert!(
        repository
            .fork_latest_turn_for_edit_checked(
                source_session_id,
                stopped_turn,
                expected_revision + 1,
                &expected_revisions,
            )
            .await
            .is_err()
    );

    let fork = repository
        .fork_latest_turn_for_edit_checked(
            source_session_id,
            stopped_turn,
            expected_revision,
            &expected_revisions,
        )
        .await
        .expect("exact edit fork");
    assert_eq!(fork.edited_turn_id, stopped_turn);
    assert_eq!(fork.editable_text, "canonical request");
    assert_eq!(fork.forked_session.status, SessionStatus::Idle);
    assert_eq!(
        fork.forked_session.project_id,
        fork.source_session.project_id
    );
    assert_eq!(fork.forked_session.cwd, fork.source_session.cwd);
    assert_eq!(fork.forked_session.model, fork.source_session.model);
    assert_eq!(fork.forked_session.base_url, fork.source_session.base_url);
    assert_eq!(
        fork.forked_session.access_mode,
        fork.source_session.access_mode
    );
    assert_eq!(
        fork.forked_session.provider_connection,
        fork.source_session.provider_connection
    );

    let source_history = store
        .protocol_event_store()
        .list_history_items_for_session(source_session_id)
        .expect("source history");
    let branch_history = store
        .protocol_event_store()
        .list_history_items_for_session(fork.forked_session.id)
        .expect("branch history");
    assert!(
        source_history
            .iter()
            .any(|item| item.turn_id() == Some(stopped_turn))
    );
    assert!(
        branch_history
            .iter()
            .any(|item| item.turn_id() == Some(first_turn))
    );
    assert!(
        !branch_history
            .iter()
            .any(|item| item.turn_id() == Some(stopped_turn))
    );
    for (table, count) in protocol_turn_table_counts(&store, fork.forked_session.id, stopped_turn) {
        assert_eq!(
            count, 0,
            "{table} leaked a stopped-turn projection into the branch"
        );
    }
    assert!(
        repository
            .durable_terminal_for_turn(source_session_id, stopped_turn)
            .await
            .expect("source terminal")
            .is_some()
    );
}

#[tokio::test]
async fn edit_fork_accepts_finished_turn_with_settled_agent_history_without_copying_authority() {
    let (store, source_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission, turn_id) = active_turn(&store, source_session_id).await;
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                source_session_id,
                admission,
                &completed_terminal(source_session_id),
                turn_id,
                None,
                None,
            )
            .await
            .expect("completed turn"),
        AdmittedTerminalCommit::Applied
    );
    let child = create_sibling_session(&store, source_session_id, "past_child").await;
    repository
        .insert_session_spawn_edge(
            source_session_id,
            source_session_id,
            child.id,
            "/root/past_child",
            "past_child",
        )
        .await
        .expect("past agent edge");
    let expected_revision = repository
        .active_turn_expectation_for_session(source_session_id)
        .await
        .expect("idle state")
        .expect("source")
        .revision();
    let expected_revisions = repository
        .origin_history_revisions_for_session_tree(source_session_id, 2)
        .expect("source and child revisions");
    let fork = repository
        .fork_latest_turn_for_edit_checked(
            source_session_id,
            turn_id,
            expected_revision,
            &expected_revisions,
        )
        .await
        .expect("finished turn edit fork");
    let branch_history = store
        .protocol_event_store()
        .list_history_items_for_session(fork.forked_session.id)
        .expect("branch history");
    assert!(
        !branch_history
            .iter()
            .any(|item| item.turn_id() == Some(turn_id))
    );
    let connection = repository.connection.lock().expect("sqlite mutex");
    let branch_edges: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM session_spawn_edges
                 WHERE parent_session_id = ?1 OR child_session_id = ?1",
            params![fork.forked_session.id.to_string()],
            |row| row.get(0),
        )
        .expect("branch edge count");
    assert_eq!(branch_edges, 0, "the fork must not inherit agent authority");
}

#[tokio::test]
async fn expired_admission_recovery_derives_terminal_after_user_turn_crash() {
    let (store, session_id) = test_repo().await;
    let (_, turn_id) = active_turn(&store, session_id).await;
    expire_and_recover_run(&store, session_id).await;

    let terminal = store
        .session_repo()
        .durable_terminal_for_turn(session_id, turn_id)
        .await
        .expect("terminal read")
        .expect("recovery terminal");
    assert_eq!(terminal.final_response_id, None);
    assert_eq!(terminal.tool_call_count, 0);
    assert_eq!(terminal.failed_tool_count, 0);
    assert_eq!(terminal.change_count, 0);
}

#[tokio::test]
async fn expired_admission_recovery_derives_response_and_failed_pending_call() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let response_id = ModelResponseId::new();
    let call_id = ToolCallId::new();
    store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id,
                assistant_text: Some("Calling the tool.".to_string()),
                assistant_protocol_sequence_no: None,
                tool_calls: vec![PendingToolCallWrite {
                    id: call_id,
                    model_call_id: "provider-call".to_string(),
                    tool_name: "read".to_string(),
                    arguments_json: serde_json::json!({"path": "README.md"}).to_string(),
                    protocol_sequence_no: None,
                }],
            },
        )
        .await
        .expect("model response");
    expire_and_recover_run(&store, session_id).await;

    let terminal = store
        .session_repo()
        .durable_terminal_for_turn(session_id, turn_id)
        .await
        .expect("terminal read")
        .expect("recovery terminal");
    assert_eq!(terminal.final_response_id, None);
    assert_eq!(terminal.tool_call_count, 1);
    assert_eq!(terminal.failed_tool_count, 1);
    assert_eq!(terminal.change_count, 0);
    assert!(
        store
            .protocol_event_store()
            .list_history_items(session_id, turn_id)
            .expect("recovered history")
            .iter()
            .any(|item| matches!(
                item.payload,
                HistoryItemPayload::ToolOutput {
                    call_id: stored_call_id,
                    status: ToolLifecycleStatus::Failed,
                    ..
                } if stored_call_id == call_id
            ))
    );
}

#[tokio::test]
async fn expired_admission_recovery_derives_completed_tool_and_change_counts() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let response_id = ModelResponseId::new();
    let call_id = ToolCallId::new();
    store
        .session_repo()
        .record_model_response_with_protocol_bundle(
            session_id,
            admission_id,
            turn_id,
            ModelResponseWrite {
                response_id,
                assistant_text: None,
                assistant_protocol_sequence_no: None,
                tool_calls: vec![PendingToolCallWrite {
                    id: call_id,
                    model_call_id: "provider-call".to_string(),
                    tool_name: "apply_patch".to_string(),
                    arguments_json: serde_json::json!({"patch": "test"}).to_string(),
                    protocol_sequence_no: None,
                }],
            },
        )
        .await
        .expect("model response");
    let durable_changes = vec![
        crate::edit::FileChange {
            id: ChangeId::new(),
            tool_call_id: call_id,
            kind: ChangeKind::Update,
            path_before: Some("a.txt".into()),
            path_after: Some("a.txt".into()),
            before_sha256: Some("before-a".to_string()),
            after_sha256: Some("after-a".to_string()),
            diff_text: "a changed".to_string(),
            summary: "updated a.txt".to_string(),
            created_at_ms: 1,
        },
        crate::edit::FileChange {
            id: ChangeId::new(),
            tool_call_id: call_id,
            kind: ChangeKind::Add,
            path_before: None,
            path_after: Some("b.txt".into()),
            before_sha256: None,
            after_sha256: Some("after-b".to_string()),
            diff_text: "b added".to_string(),
            summary: "added b.txt".to_string(),
            created_at_ms: 1,
        },
    ];
    store
        .change_repo()
        .insert_changes(&durable_changes)
        .await
        .expect("durable file-change evidence");
    let changes = durable_changes
        .iter()
        .map(|change| crate::edit::ChangeSummary {
            change_id: change.id,
            kind: change.kind,
            path_before: change.path_before.clone(),
            path_after: change.path_after.clone(),
        })
        .collect();
    store
        .session_repo()
        .complete_tool_call_with_file_changes_protocol_bundle(
            session_id,
            admission_id,
            call_id,
            crate::tool::ToolName::ApplyPatch,
            "apply_patch",
            serde_json::json!({"success": true}),
            "updated files",
            None,
            changes,
            turn_id,
            None,
            None,
        )
        .await
        .expect("tool settlement")
        .expect("tool settled with canonical changes");
    expire_and_recover_run(&store, session_id).await;

    let terminal = store
        .session_repo()
        .durable_terminal_for_turn(session_id, turn_id)
        .await
        .expect("terminal read")
        .expect("recovery terminal");
    assert_eq!(terminal.final_response_id, None);
    assert_eq!(terminal.tool_call_count, 1);
    assert_eq!(terminal.failed_tool_count, 0);
    assert_eq!(terminal.change_count, 2);
}

#[tokio::test]
async fn current_turn_terminal_finish_drains_pending_mail_without_resampling() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let stored_active_mail = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "new evidence".to_string(),
                trigger_turn: false,
            },
            true,
        )
        .expect("active mail append");
    assert!(!stored_active_mail.schedule_turn);
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history before delivery")
            .iter()
            .all(|item| item.id != stored_active_mail.history_item_id),
        "enqueue must not write model-visible history"
    );
    let terminal = completed_terminal(session_id);
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &terminal,
                turn_id,
                None,
                None,
            )
            .await
            .expect("terminal CAS with finish-drain"),
        AdmittedTerminalCommit::Applied
    );
    let active_mail = store
        .protocol_event_store()
        .history_items_by_id(session_id, &[stored_active_mail.history_item_id])
        .expect("delivered history")
        .into_iter()
        .next()
        .expect("active mail history");
    assert_eq!(
        active_mail.scope,
        crate::protocol::HistoryScope::Turn { turn_id }
    );
    let append_after_terminal = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "too late".to_string(),
                trigger_turn: false,
            },
            true,
        );
    assert!(append_after_terminal.is_err());
    assert_eq!(
        store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history")
            .iter()
            .filter(|item| matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_terminal_finish_drains_eligible_pending_mail_without_resampling() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let queued = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "evidence racing a failed terminal".to_string(),
                trigger_turn: false,
            },
            true,
        )
        .expect("eligible pending mail");

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &failed_terminal(session_id, "provider failed"),
                turn_id,
                None,
                None,
            )
            .await
            .expect("failed terminal with finish-drain"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        store
            .protocol_event_store()
            .history_items_by_id(session_id, &[queued.history_item_id])
            .expect("finish-drained failure history")
            .len(),
        1
    );
    assert!(
        !store
            .session_repo()
            .has_pending_agent_mailbox_messages(session_id)
            .expect("mailbox after failure finish-drain")
    );
}

#[tokio::test]
async fn inactive_recipient_mail_stays_out_of_history_until_a_safe_turn_delivery() {
    let (store, session_id) = test_repo().await;
    let (admission_id, completed_turn_id) = active_turn(&store, session_id).await;
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &completed_terminal(session_id),
                completed_turn_id,
                None,
                None,
            )
            .await
            .expect("terminal"),
        AdmittedTerminalCommit::Applied
    );

    let communication_id = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "evidence for a future continuation".to_string(),
                trigger_turn: false,
            },
            false,
        )
        .expect("inactive recipient communication");
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history before continuation")
            .iter()
            .all(|item| item.id != communication_id.history_item_id)
    );
    let queued = store
        .session_repo()
        .agent_mailbox_communications_by_id(session_id, &[communication_id.history_item_id])
        .expect("durable pending mailbox");
    assert_eq!(queued.len(), 1);

    let continuation_turn_id = TurnId::new();
    let continuation = store
        .session_repo()
        .admit_session_turn(session_id, continuation_turn_id)
        .await
        .expect("continuation admission")
        .expect("idle session admitted");
    let delivered = store
        .session_repo()
        .deliver_pending_agent_mail_for_admitted_turn(
            session_id,
            continuation.admission_id,
            continuation_turn_id,
            128,
        )
        .expect("deliver queued evidence");
    assert_eq!(
        delivered.history_item_ids,
        vec![communication_id.history_item_id]
    );
    let communication = store
        .protocol_event_store()
        .history_items_by_id(session_id, &[communication_id.history_item_id])
        .expect("delivered history")
        .into_iter()
        .next()
        .expect("communication item");

    assert_eq!(
        communication.scope,
        crate::protocol::HistoryScope::Turn {
            turn_id: continuation_turn_id
        }
    );
    assert!(matches!(
        communication.payload,
        HistoryItemPayload::InterAgentCommunication { .. }
    ));
    assert!(
        store
            .session_repo()
            .durable_terminal_for_turn(session_id, completed_turn_id)
            .await
            .expect("terminal read")
            .is_some()
    );
}

#[tokio::test]
async fn running_recipient_followup_survives_interruption_until_next_safe_delivery() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (admission_id, turn_id) = active_turn(&store, child.id).await;
    let repository = store.session_repo();
    let followup = repository
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/child",
                    "/root",
                    "follow up before the current turn reaches a safe mailbox boundary",
                ),
                trigger_turn: true,
            },
            true,
        )
        .expect("running-recipient follow-up");
    assert!(!followup.schedule_turn);
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                admission_id,
                &agent_interrupted_terminal(child.id),
                turn_id,
                None,
                None,
            )
            .await
            .expect("immediate child interruption"),
        AdmittedTerminalCommit::Applied
    );

    assert_eq!(
        repository
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending follow-up after interruption"),
        Some(followup.history_item_id)
    );
    let continuation_turn_id = TurnId::new();
    let continuation = repository
        .admit_agent_triggered_turn(child.id, continuation_turn_id, followup.history_item_id)
        .await
        .expect("recoverable follow-up admission")
        .expect("follow-up admitted");
    let delivered = repository
        .deliver_pending_agent_mail_for_admitted_turn(
            child.id,
            continuation.admission_id,
            continuation_turn_id,
            128,
        )
        .expect("follow-up delivery");
    assert_eq!(delivered.history_item_ids, vec![followup.history_item_id]);
    assert_eq!(
        repository
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending trigger after delivery"),
        None
    );
    let history = store
        .protocol_event_store()
        .history_items_by_id(child.id, &[followup.history_item_id])
        .expect("delivered follow-up history");
    assert!(matches!(
        history.as_slice(),
        [item]
            if item.scope
                == (HistoryScope::Turn {
                    turn_id: continuation_turn_id,
                })
    ));
}

#[tokio::test]
async fn next_turn_phase_allows_ordinary_mail_to_remain_pending_after_success() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let queued = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "arrived after the final sample".to_string(),
                trigger_turn: false,
            },
            true,
        )
        .expect("ordinary pending mail");

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                session_id,
                admission_id,
                &completed_terminal(session_id),
                turn_id,
                None,
                false,
                None,
            )
            .await
            .expect("NextTurn terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        store
            .session_repo()
            .has_pending_agent_mailbox_messages(session_id)
            .expect("pending mailbox")
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history")
            .iter()
            .all(|item| item.id != queued.history_item_id)
    );
}

#[tokio::test]
async fn next_turn_phase_leaves_direct_child_final_pending_without_blocking_terminal() {
    let (store, root_session_id) = test_repo().await;
    let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, None),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("child terminal"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("handoff query")
        .expect("pending child FINAL");

    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                root_session_id,
                root_admission_id,
                &completed_terminal(root_session_id),
                root_turn_id,
                None,
                false,
                None,
            )
            .await
            .expect("root NextTurn terminal"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        store
            .session_repo()
            .has_pending_agent_mailbox_messages(root_session_id)
            .expect("pending FINAL")
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items(root_session_id, root_turn_id)
            .expect("terminal turn history")
            .iter()
            .all(|item| item.id != handoff.history_item_id)
    );
    assert_eq!(
        store
            .session_repo()
            .agent_mailbox_communications_by_id(root_session_id, &[handoff.history_item_id],)
            .expect("pending FINAL")
            .len(),
        1
    );
}

#[tokio::test]
async fn required_only_delivery_keeps_late_ordinary_mail_pending_when_forced_sample_is_final() {
    let (store, root_session_id) = test_repo().await;
    let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let ordinary = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            root_session_id,
            InterAgentCommunication {
                author: "/root/child".to_string(),
                recipient: "/root".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::Message,
                    "/root",
                    "/root/child",
                    "ordinary mail after the owner's visible final",
                ),
                trigger_turn: false,
            },
            true,
        )
        .expect("late ordinary mail");
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, None),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("child terminal"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("handoff query")
        .expect("required child FINAL");

    let required = store
        .session_repo()
        .deliver_pending_agent_mail_for_admitted_turn_with_selector(
            root_session_id,
            root_admission_id,
            root_turn_id,
            AgentMailboxDeliverySelector::RequiredChildResultsOnly,
            128,
        )
        .expect("required-only safe boundary");
    assert_eq!(required.history_item_ids, vec![handoff.history_item_id]);
    assert!(!required.has_more);
    assert_eq!(
        store
            .session_repo()
            .agent_mailbox_communications_by_id(root_session_id, &[ordinary.history_item_id],)
            .expect("ordinary mail remains queued")
            .len(),
        1
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("owner history")
            .iter()
            .all(|item| item.id != ordinary.history_item_id)
    );
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event_and_mailbox_phase(
                root_session_id,
                root_admission_id,
                &completed_terminal_for_response(root_session_id, None),
                root_turn_id,
                None,
                false,
                None,
            )
            .await
            .expect("terminal after required result"),
        AdmittedTerminalCommit::Applied
    );
    assert!(
        store
            .session_repo()
            .has_pending_agent_mailbox_messages(root_session_id)
            .expect("next-turn ordinary mail")
    );
}

#[tokio::test]
async fn current_turn_all_after_required_tool_call_delivers_late_ordinary_mail() {
    let (store, root_session_id) = test_repo().await;
    let (root_admission_id, root_turn_id) = active_turn(&store, root_session_id).await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (child_admission_id, child_turn_id) = active_turn(&store, child.id).await;
    let ordinary = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            root_session_id,
            InterAgentCommunication {
                author: "/root/child".to_string(),
                recipient: "/root".to_string(),
                content: "ordinary mail queued beside required FINAL".to_string(),
                trigger_turn: false,
            },
            true,
        )
        .expect("late ordinary mail");
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                child_admission_id,
                &completed_terminal_for_response(child.id, None),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("child terminal"),
        AdmittedTerminalCommit::Applied
    );
    let handoff = store
        .session_repo()
        .agent_completion_handoff(child.id, child_turn_id)
        .expect("handoff query")
        .expect("required child FINAL");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn_with_selector(
                root_session_id,
                root_admission_id,
                root_turn_id,
                AgentMailboxDeliverySelector::RequiredChildResultsOnly,
                128,
            )
            .expect("host-forced required sample")
            .history_item_ids,
        vec![handoff.history_item_id]
    );

    // The forced sample emitted a tool call. Codex reopens the next safe
    // boundary to every current-turn mailbox item.
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn_with_selector(
                root_session_id,
                root_admission_id,
                root_turn_id,
                AgentMailboxDeliverySelector::AllPending,
                128,
            )
            .expect("safe boundary after required-sample tool call")
            .history_item_ids,
        vec![ordinary.history_item_id]
    );
    assert!(
        !store
            .session_repo()
            .has_pending_agent_mailbox_messages(root_session_id)
            .expect("mailbox after tool boundary")
    );
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("owner history")
            .iter()
            .any(|item| item.id == ordinary.history_item_id)
    );
}

#[tokio::test]
async fn delivered_followup_is_not_retriggered_after_interrupt_or_restart() {
    let (store, root_session_id) = test_repo().await;
    let child =
        retained_test_agent(&store, root_session_id, root_session_id, "/root", "child").await;
    let (admission_id, turn_id) = active_turn(&store, child.id).await;
    let followup = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            child.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/child".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/child",
                    "/root",
                    "deliver before interruption",
                ),
                trigger_turn: true,
            },
            true,
        )
        .expect("running follow-up");
    let delivered = store
        .session_repo()
        .deliver_pending_agent_mail_for_admitted_turn(child.id, admission_id, turn_id, 128)
        .expect("safe delivery");
    assert_eq!(delivered.history_item_ids, vec![followup.history_item_id]);
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.id,
                admission_id,
                &agent_interrupted_terminal(child.id),
                turn_id,
                None,
                None,
            )
            .await
            .expect("interrupt after delivery"),
        AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        store
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("pending trigger"),
        None
    );
    assert_eq!(
        store
            .session_repo()
            .admit_agent_triggered_turn(child.id, TurnId::new(), followup.history_item_id)
            .await
            .expect("delivered trigger admission"),
        None
    );

    let reopened_sqlite = SqliteStore::open(store.paths()).expect("reopen store");
    reopened_sqlite.migrate().expect("validate reopened store");
    let reopened = StoreBundle::new(reopened_sqlite);
    assert_eq!(
        reopened
            .session_repo()
            .pending_agent_trigger_history_item_id(child.id)
            .expect("reopened pending trigger"),
        None
    );
    assert_eq!(
        reopened
            .protocol_event_store()
            .list_history_items_for_session(child.id)
            .expect("reopened child history")
            .iter()
            .filter(|item| item.id == followup.history_item_id)
            .count(),
        1
    );
}

#[tokio::test]
async fn rollback_targets_only_real_turns_and_preserves_all_idle_mail() {
    let (store, session_id) = test_repo().await;
    let (admission_id, completed_turn_id) = active_turn(&store, session_id).await;
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &completed_terminal(session_id),
                completed_turn_id,
                None,
                None,
            )
            .await
            .expect("terminal"),
        AdmittedTerminalCommit::Applied
    );
    let first_mail_id = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "first future evidence".to_string(),
                trigger_turn: false,
            },
            false,
        )
        .expect("first inactive mail");
    let second_mail_id = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "second future evidence".to_string(),
                trigger_turn: false,
            },
            false,
        )
        .expect("second inactive mail");
    let rolled_back = store
        .session_repo()
        .rollback_session_transaction(session_id, 1)
        .await
        .expect("rollback latest real turn");
    assert_eq!(rolled_back.dropped_turn_ids, vec![completed_turn_id]);
    let queued = store
        .session_repo()
        .agent_mailbox_communications_by_id(
            session_id,
            &[
                first_mail_id.history_item_id,
                second_mail_id.history_item_id,
            ],
        )
        .expect("mailbox after rollback");
    assert_eq!(queued.len(), 2);
    assert!(
        store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("history after rollback")
            .iter()
            .all(|item| item.id != first_mail_id.history_item_id
                && item.id != second_mail_id.history_item_id)
    );
    assert!(
        store
            .session_repo()
            .durable_terminal_for_turn(session_id, completed_turn_id)
            .await
            .expect("completed terminal read")
            .is_none(),
        "rollback must remove the selected real turn without consuming session mail"
    );
}

#[tokio::test]
async fn rollback_rejects_turn_owned_by_terminalized_owner_resume_request() {
    for tree_stopped in [false, true] {
        let (store, root_session_id) = test_repo().await;
        let repository = store.session_repo();
        let (owner, requests) = nested_owner_resume_fixture(&store, root_session_id, 1).await;
        let request_id = requests[0].request_id;
        let owner_turn = TurnId::new();
        let owner_admission = repository
            .admit_owner_resume_turn(owner.id, owner_turn, request_id)
            .await
            .expect("OwnerResume admission")
            .expect("OwnerResume admitted");
        if tree_stopped {
            repository
                .record_agent_tree_stop_fence(
                    root_session_id,
                    crate::protocol::TurnInterruptionCause::UserStop,
                )
                .await
                .expect("record ancestor OwnerResume tree-stop fence")
                .expect("ancestor OwnerResume tree-stop fence");
        }
        let terminal = if tree_stopped {
            tree_stopped_terminal(owner.id)
        } else {
            agent_interrupted_terminal(owner.id)
        };
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    owner.id,
                    owner_admission.admission_id,
                    &terminal,
                    owner_turn,
                    None,
                    None,
                )
                .await
                .expect("owner interruption"),
            AdmittedTerminalCommit::Applied
        );

        let error = repository
            .rollback_session_transaction(owner.id, 1)
            .await
            .expect_err("OwnerResume-owned turn rollback must be rejected");
        assert!(
            error
                .to_string()
                .contains("durable OwnerResume request claim"),
            "unexpected rollback rejection: {error}"
        );
        assert!(
            !store
                .protocol_event_store()
                .list_runtime_events(owner.id, owner_turn)
                .expect("retained OwnerResume turn")
                .is_empty()
        );
        let (state, claimed_turn_id) = repository
            .connection
            .lock()
            .expect("sqlite mutex")
            .query_row(
                "SELECT state, claimed_turn_id
                     FROM agent_owner_resume_requests
                     WHERE owner_session_id = ?1 AND source_history_item_id = ?2",
                params![owner.id.to_string(), request_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("retained OwnerResume row");
        assert!(
            matches!(state.as_str(), "resolved" | "cancelled"),
            "unexpected retained OwnerResume state: {state}"
        );
        assert_eq!(claimed_turn_id, Some(owner_turn.to_string()));
    }
}

#[tokio::test]
async fn rollback_rejects_a_turn_owned_by_an_explicit_wake_claim() {
    let (store, root_session_id) = test_repo().await;
    let (child, history_item_id, _) =
        spawn_pending_child(&store, root_session_id, "rollback_claim").await;
    let repository = store.session_repo();
    let turn_id = TurnId::new();
    repository
        .admit_agent_triggered_turn(child.id, turn_id, history_item_id)
        .await
        .expect("explicit admission")
        .expect("explicit wake admitted");
    assert!(matches!(
        repository
            .settle_agent_execution_wake_with_terminal(
                child.id,
                AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id),
                pre_admission_agent_interrupted_terminal(),
            )
            .expect("exact wake settlement"),
        AgentExecutionWakeTerminalSettlement::Applied {
            turn_id: settled_turn_id,
            ..
        } if settled_turn_id == turn_id
    ));
    let root_admission_id = repository
        .connection
        .lock()
        .expect("sqlite mutex")
        .query_row(
            "SELECT active_run_id FROM sessions WHERE id = ?1",
            [root_session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .expect("root admission id")
        .parse::<AdmissionId>()
        .expect("valid root admission id");
    let root_target = repository
        .captured_running_terminal_target(root_session_id)
        .await
        .expect("capture root target")
        .expect("root remains admitted");
    assert!(
        repository
            .terminalize_captured_running_session_with_protocol_event(
                root_session_id,
                &completed_terminal_for_response(root_session_id, None),
                root_target,
            )
            .await
            .expect("complete detached root")
    );
    assert!(
        repository
            .release_stopped_run_admission(root_session_id, root_admission_id)
            .await
            .expect("release retained root admission")
    );

    let error = repository
        .rollback_session_transaction(child.id, 1)
        .await
        .expect_err("explicit wake identity must survive rollback");
    assert!(
        error.to_string().contains("explicit agent wake claim"),
        "unexpected rollback rejection: {error}"
    );
    assert!(
        repository
            .durable_terminal_for_turn(child.id, turn_id)
            .await
            .expect("retained explicit terminal")
            .is_some()
    );
}

#[tokio::test]
async fn rollback_rejects_a_turn_containing_delivered_mailbox_input() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let stored = store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            session_id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root".to_string(),
                content: "immutable delivered input".to_string(),
                trigger_turn: false,
            },
            true,
        )
        .expect("pending self mail");
    assert_eq!(
        store
            .session_repo()
            .deliver_pending_agent_mail_for_admitted_turn(session_id, admission_id, turn_id, 128,)
            .expect("deliver self mail")
            .history_item_ids,
        vec![stored.history_item_id]
    );
    assert_eq!(
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &agent_interrupted_terminal(session_id),
                turn_id,
                None,
                None,
            )
            .await
            .expect("close delivered turn"),
        AdmittedTerminalCommit::Applied
    );

    let error = store
        .session_repo()
        .rollback_session_transaction(session_id, 1)
        .await
        .expect_err("delivered mailbox lifecycle must block rollback");
    assert!(error.to_string().contains("mailbox delivery is immutable"));
    let repository = store.session_repo();
    let connection = repository.connection.lock().expect("sqlite mutex");
    assert_eq!(
        connection
            .query_row(
                "SELECT state
                     FROM agent_mailbox_messages
                     WHERE id = ?1",
                params![stored.history_item_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .expect("mailbox state after rejected rollback"),
        "delivered"
    );
    assert!(
        connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1
                     FROM protocol_history_items
                     WHERE id = ?1 AND turn_id = ?2
                 )",
                params![stored.history_item_id.to_string(), turn_id.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .expect("history after rejected rollback")
    );
}

#[tokio::test]
async fn admitted_terminal_is_first_writer_and_is_rehydrated_as_one_typed_value() {
    let (store, session_id) = test_repo().await;
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let repo = store.session_repo();
    let event = completed_terminal(session_id);
    assert_eq!(
        repo.terminalize_admitted_turn_with_protocol_event(
            session_id,
            admission_id,
            &event,
            turn_id,
            None,
            None,
        )
        .await
        .expect("terminalize"),
        AdmittedTerminalCommit::Applied
    );
    let durable = repo
        .durable_terminal_for_turn(session_id, turn_id)
        .await
        .expect("read terminal")
        .expect("terminal");
    assert!(matches!(durable.outcome, TurnTerminalOutcome::Completed));
    assert_eq!(durable.summary(), "completed");
    assert_eq!(
        store
            .protocol_event_store()
            .list_runtime_events(session_id, turn_id)
            .expect("events")
            .iter()
            .filter(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
            .count(),
        1
    );
    assert_eq!(
        repo.terminalize_admitted_turn_with_protocol_event(
            session_id,
            admission_id,
            &completed_terminal(session_id),
            turn_id,
            None,
            None,
        )
        .await
        .expect("second terminal attempt"),
        AdmittedTerminalCommit::NotOwned
    );
    assert_eq!(
        repo.durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("read terminal")
            .expect("terminal")
            .summary(),
        "completed"
    );
}

#[tokio::test]
async fn captured_terminal_target_cannot_terminalize_a_replacement_turn() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (first_admission_id, first_turn_id) = active_turn(&store, session_id).await;
    let first_target = repository
        .captured_running_terminal_target(session_id)
        .await
        .expect("capture first target")
        .expect("first running target");
    assert_eq!(
        repository
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                first_admission_id,
                &completed_terminal(session_id),
                first_turn_id,
                None,
                None,
            )
            .await
            .expect("complete first turn"),
        AdmittedTerminalCommit::Applied
    );

    let second_turn_id = TurnId::new();
    repository
        .admit_session_turn(session_id, second_turn_id)
        .await
        .expect("replacement admission")
        .expect("replacement admitted");
    assert!(
        !repository
            .terminalize_captured_running_session_with_protocol_event(
                session_id,
                &failed_terminal(session_id, "stale stop target"),
                first_target,
            )
            .await
            .expect("stale target is a clean CAS miss")
    );
    assert_eq!(
        repository
            .fresh_running_turn_for_session(session_id)
            .await
            .expect("replacement turn"),
        Some(second_turn_id)
    );
    assert!(
        repository
            .durable_terminal_for_turn(session_id, second_turn_id)
            .await
            .expect("replacement terminal lookup")
            .is_none()
    );
}

#[tokio::test]
async fn captured_terminal_target_survives_same_owner_lease_renewal() {
    let (store, session_id) = test_repo().await;
    let repository = store.session_repo();
    let (admission_id, turn_id) = active_turn(&store, session_id).await;
    let target = repository
        .captured_running_terminal_target(session_id)
        .await
        .expect("capture target")
        .expect("running target");
    assert!(matches!(
        repository
            .renew_admitted_run_lease_at(
                session_id,
                admission_id,
                turn_id,
                SystemClock::now_ms(),
                RUN_ADMISSION_LEASE_DURATION_MS * 4,
            )
            .await
            .expect("renew same owner"),
        RunAdmissionLeaseRenewalOutcome::Renewed
    ));
    assert!(
        repository
            .terminalize_captured_running_session_with_protocol_event(
                session_id,
                &failed_terminal(session_id, "stop after renewal"),
                target,
            )
            .await
            .expect("terminalize renewed owner")
    );
    assert!(
        repository
            .durable_terminal_for_turn(session_id, turn_id)
            .await
            .expect("terminal lookup")
            .is_some()
    );
}

#[tokio::test]
async fn terminal_recovery_requires_the_captured_matching_owner() {
    let (store, null_turn_session_id) = test_repo().await;
    let repository = store.session_repo();
    repository
        .inject_raw_runtime_state_for_corruption_test(
            null_turn_session_id,
            "running",
            None,
            None,
            None,
        )
        .expect("create turnless running fixture");
    let recovered_turn_id = TurnId::new();
    let error = repository
        .captured_running_terminal_target(null_turn_session_id)
        .await
        .expect_err("turnless recovery must fail closed");
    assert!(error.to_string().contains("durable run admission"));
    assert!(
        repository.get_session(null_turn_session_id).await.is_err(),
        "ordinary reads must reject the same invalid owner state"
    );
    assert!(
        repository
            .durable_terminal_for_turn(null_turn_session_id, recovered_turn_id)
            .await
            .expect("turnless terminal lookup")
            .is_none()
    );

    let (store, owned_session_id) = test_repo().await;
    let repository = store.session_repo();
    let (_admission_id, active_turn_id) = active_turn(&store, owned_session_id).await;
    let owned_target = repository
        .captured_running_terminal_target(owned_session_id)
        .await
        .expect("capture owned target")
        .expect("owned running target");
    let foreign_session = create_sibling_session(&store, owned_session_id, "foreign owner").await;
    let (_foreign_admission_id, foreign_turn_id) = active_turn(&store, foreign_session.id).await;
    let foreign_target = repository
        .captured_running_terminal_target(foreign_session.id)
        .await
        .expect("capture foreign target")
        .expect("foreign running target");
    assert_ne!(foreign_turn_id, active_turn_id);
    assert!(
        !repository
            .terminalize_captured_running_session_with_protocol_event(
                owned_session_id,
                &failed_terminal(owned_session_id, "foreign turn"),
                foreign_target,
            )
            .await
            .expect("reject foreign recovery turn")
    );
    assert_eq!(
        repository
            .get_session(owned_session_id)
            .await
            .expect("owned session")
            .status,
        SessionStatus::Running
    );
    assert!(
        repository
            .durable_terminal_for_turn(owned_session_id, foreign_turn_id)
            .await
            .expect("foreign terminal lookup")
            .is_none()
    );
    assert!(
        repository
            .recover_captured_running_session_with_protocol_event(
                owned_session_id,
                &failed_terminal(owned_session_id, "orphaned run"),
                owned_target,
            )
            .await
            .expect("recover the exact durable active turn")
    );
    assert_eq!(
        repository
            .get_session(owned_session_id)
            .await
            .expect("recovered session")
            .status,
        SessionStatus::Failed
    );
    assert!(
        repository
            .durable_terminal_for_turn(owned_session_id, active_turn_id)
            .await
            .expect("active terminal lookup")
            .is_some()
    );
}

#[test]
fn terminal_writer_rejects_non_terminal_events_and_invalid_counts() {
    let session_id = SessionId::new();
    let non_terminal = RunEvent::SessionStarted {
        session_id,
        title: "test".to_string(),
    };
    assert!(validate_terminal_event(session_id, &non_terminal).is_err());
    let invalid_counts = RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(crate::session::model::DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 1,
            change_count: 0,
            metrics: Default::default(),
        }),
    };
    assert!(validate_terminal_event(session_id, &invalid_counts).is_err());
}
