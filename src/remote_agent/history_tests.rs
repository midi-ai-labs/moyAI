use super::*;
use crate::app::{App, AppBootstrap};
use crate::config::{AccessMode, ResolvedConfig};
use crate::protocol::{
    HistoryItem, HistoryItemId, HistoryScope, ModelResponseId, TurnItem, TurnItemId,
    TurnTerminalOutcome, UserInputItem, UserTurn,
};
use crate::remote_agent::{RemoteJobParent, RemoteTaskRequest};
use crate::session::{NewSession, ToolCallId};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use camino::Utf8PathBuf;
use serde_json::json;
use sha2::{Digest, Sha256};

async fn fixture() -> (tempfile::TempDir, App, NewSession) {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let paths = StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let app = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
        &workspace,
        StoreBundle::new(sqlite),
        ResolvedConfig::default(),
    )
    .await
    .unwrap();
    let draft = NewSession {
        project_id: app.workspace.project_id,
        title: "MCP監視試験".into(),
        cwd: workspace,
        model: "fixture-model".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        access_mode: AccessMode::Default,
        provider_connection: None,
    };
    (temp, app, draft)
}

fn reference(session_id: SessionId, turn_id: TurnId, key: &str) -> StoredDeviceReference {
    StoredDeviceReference {
        id: Ulid::new(),
        session_id,
        turn_id,
        device_id: "device-b".into(),
        profile_id: "receiver".into(),
        root_task_id: "root-task".into(),
        request_key: crate::device_network::history_request_key(session_id, turn_id, key),
        prompt_hash: format!("{:x}", Sha256::digest(b"fixture")),
        parent_grant_id: None,
        parent_job_id: None,
        peer: crate::device_network::DirectoryPeer {
            device_id: "device-b".into(),
            label: "WinB 日本語".into(),
            profile_id: "receiver".into(),
            name: "temp".into(),
            endpoint: "https://127.0.0.1:9/mcp".into(),
            mode: "agent".into(),
            scope_id: "scope".into(),
            certificate_pem: "CERTIFICATE_MUST_NOT_EXPORT".into(),
            certificate_sha256: "a".repeat(64),
        },
        claims: None,
        job_id: Some(Ulid::new().to_string()),
        state: "running".into(),
        stop_status: "none".into(),
        result: None,
    }
}

fn history(
    app: &App,
    session: SessionId,
    turn: TurnId,
    sequence: i64,
    payload: HistoryItemPayload,
) {
    app.store
        .protocol_event_store()
        .seed_history_item_for_test(&HistoryItem {
            id: HistoryItemId::new(),
            session_id: session,
            scope: HistoryScope::Turn { turn_id: turn },
            sequence_no: sequence,
            created_at_ms: sequence + 1000,
            payload,
        })
        .unwrap();
}

fn user(text: &str) -> HistoryItemPayload {
    HistoryItemPayload::UserTurn {
        content: vec![ContentPart::Text { text: text.into() }],
        prompt_dispatch: None,
        editor_context: None,
    }
}

fn tool_call(
    reference: &StoredDeviceReference,
    key: &str,
    call: ToolCallId,
    prompt: &str,
) -> HistoryItemPayload {
    HistoryItemPayload::ToolCall { call_id: call, response_id: ModelResponseId::new(), model_call_id: "fixture".into(), tool_name: "mcp_call".into(), arguments_json: serde_json::json!({"server_id":crate::device_network::history_server_id(&reference.peer),"tool_name":"delegate_task","arguments":{"request_key":key,"prompt":prompt,"authorization":"SECRET_MUST_NOT_EXPORT"}}).to_string() }
}

fn tool_output(call: ToolCallId, text: &str) -> HistoryItemPayload {
    HistoryItemPayload::ToolOutput {
        call_id: call,
        status: crate::protocol::ToolLifecycleStatus::Completed,
        title: "MCP結果".into(),
        output_text: text.into(),
        metadata: serde_json::json!({"secret":"METADATA_MUST_NOT_EXPORT"}),
        success: Some(true),
    }
}

fn wait_call(call: ToolCallId, jobs: &[String]) -> HistoryItemPayload {
    HistoryItemPayload::ToolCall {
        call_id: call,
        response_id: ModelResponseId::new(),
        model_call_id: "wait-fixture".into(),
        tool_name: "wait_remote_tasks".into(),
        arguments_json: serde_json::json!({"job_ids":jobs,"timeout_ms":600_000,
            "after":{"UNRELATED_CURSOR":"MUST_NOT_EXPORT_CURSOR"}})
        .to_string(),
    }
}

fn wait_job(reference: &StoredDeviceReference, result: &str) -> Value {
    serde_json::json!({
        "reference_id":reference.id.to_string(),"job_id":reference.job_id,
        "device_id":reference.device_id,"profile_id":reference.profile_id,
        "state":"completed","stop_status":"none","result":result,
        "private_extra":"MUST_NOT_EXPORT_EXTRA",
    })
}

#[tokio::test]
async fn instruction_history_projects_wait_receipts_across_turns_without_sibling_results() {
    let (_temp, app, draft) = fixture().await;
    let session = app
        .store
        .session_repo()
        .create_session(draft.clone())
        .await
        .unwrap();
    let foreign = app
        .store
        .session_repo()
        .create_session(draft)
        .await
        .unwrap();
    let original_turn = TurnId::new();
    let later_turn = TurnId::new();
    let target = reference(session.id, original_turn, "wanted-wait");
    let mut sibling = reference(session.id, original_turn, "sibling-wait");
    sibling.device_id = "UNRELATED_DEVICE_C".into();
    sibling.peer.device_id = sibling.device_id.clone();
    app.store
        .remote_job_store()
        .accept_device_reference(&target)
        .unwrap();
    app.store
        .remote_job_store()
        .accept_device_reference(&sibling)
        .unwrap();
    let ids = vec![
        target.job_id.clone().unwrap(),
        sibling.job_id.clone().unwrap(),
    ];
    let initial_call = ToolCallId::new();
    let later_call = ToolCallId::new();
    for (turn, call, result) in [
        (original_turn, initial_call, "first observed B"),
        (later_turn, later_call, "later observed B 日本語"),
    ] {
        history(&app, session.id, turn, 0, wait_call(call, &ids));
        history(
            &app,
            session.id,
            turn,
            1,
            tool_output(
                call,
                &serde_json::json!({
                    "interrupted":false,"timed_out":false,"requested_job_ids":ids,
                    "jobs":[wait_job(&target,result),wait_job(&sibling,"UNRELATED_C_RESULT")],
                    "message":"MUST_NOT_EXPORT_MESSAGE","cursor":{"UNRELATED_CURSOR":"private"}
                })
                .to_string(),
            ),
        );
    }
    history(
        &app,
        session.id,
        later_turn,
        2,
        user("UNRELATED_LATER_USER_INPUT"),
    );
    let foreign_call = ToolCallId::new();
    history(
        &app,
        foreign.id,
        original_turn,
        0,
        wait_call(foreign_call, &ids),
    );
    history(
        &app,
        foreign.id,
        original_turn,
        1,
        tool_output(
            foreign_call,
            &serde_json::json!({"jobs":[wait_job(&target,"UNRELATED_FOREIGN_RESULT")]}).to_string(),
        ),
    );
    let sibling_call = ToolCallId::new();
    history(
        &app,
        session.id,
        later_turn,
        3,
        wait_call(sibling_call, &[sibling.job_id.clone().unwrap()]),
    );
    history(
        &app,
        session.id,
        later_turn,
        4,
        tool_output(
            sibling_call,
            &serde_json::json!({"jobs":[wait_job(&target,"UNRELATED_UNREQUESTED_RESULT")]})
                .to_string(),
        ),
    );
    let service = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let detail = service
        .history_detail(McpHistoryDirection::Instruction, &target.id.to_string())
        .await
        .unwrap();
    for expected in [
        "wait_remote_tasks",
        "first observed B",
        "later observed B 日本語",
        "この委任の対象だけ",
        &initial_call.to_string(),
        &later_call.to_string(),
        &later_turn.to_string(),
    ] {
        assert!(detail.markdown.contains(expected), "missing {expected}");
    }
    for private in [
        "UNRELATED_",
        "MUST_NOT_EXPORT_",
        sibling.job_id.as_deref().unwrap(),
        &sibling.id.to_string(),
        &foreign_call.to_string(),
        &sibling_call.to_string(),
    ] {
        assert!(!detail.markdown.contains(private), "leaked {private}");
    }
    assert_eq!(detail.row.updated_at_ms, Some(1001));
    assert!(
        !detail.row.result_received,
        "history does not rewrite the observation owner"
    );
    assert!(!detail.truncated);
}

#[tokio::test]
async fn instruction_history_wait_requires_exact_reference_and_never_recovers_truncated_body() {
    let (_temp, app, draft) = fixture().await;
    let session = app
        .store
        .session_repo()
        .create_session(draft)
        .await
        .unwrap();
    let turn = TurnId::new();
    let target = reference(session.id, turn, "wait-truncated");
    app.store
        .remote_job_store()
        .accept_device_reference(&target)
        .unwrap();
    let ids = vec![target.job_id.clone().unwrap()];
    for (index, changed_field) in ["reference_id", "device_id", "profile_id", "job_id"]
        .into_iter()
        .enumerate()
    {
        let call = ToolCallId::new();
        let mut wrong = wait_job(&target, "UNRELATED_MISMATCHED_RESULT");
        wrong[changed_field] = "UNRELATED_IDENTITY".into();
        history(
            &app,
            session.id,
            turn,
            index as i64 * 2,
            wait_call(call, &ids),
        );
        history(
            &app,
            session.id,
            turn,
            index as i64 * 2 + 1,
            tool_output(call, &serde_json::json!({"jobs":[wrong]}).to_string()),
        );
    }
    let truncated_call = ToolCallId::new();
    history(&app, session.id, turn, 20, wait_call(truncated_call, &ids));
    let mut output = tool_output(truncated_call, "UNRELATED_RAW_PREVIEW [output truncated]");
    if let HistoryItemPayload::ToolOutput {
        metadata, title, ..
    } = &mut output
    {
        *title = "UNRELATED_TITLE".into();
        *metadata = serde_json::json!({"success":true,"tool_metadata":{
            "truncated":true,"interrupted":false,"timed_out":true,
            "jobs":[wait_job(&target,"UNRELATED_METADATA_BODY")],"secret":"UNRELATED_SECRET"}});
    }
    history(&app, session.id, turn, 21, output);
    let flat_call = ToolCallId::new();
    history(&app, session.id, turn, 24, wait_call(flat_call, &ids));
    let mut flat_output = tool_output(flat_call, "UNRELATED_LEGACY_PREVIEW [output truncated]");
    if let HistoryItemPayload::ToolOutput { metadata, .. } = &mut flat_output {
        let mut row = wait_job(&target, "UNRELATED_LEGACY_METADATA_BODY");
        row["state"] = "awaiting_approval".into();
        *metadata = serde_json::json!({"truncated":true,"jobs":[row]});
    }
    history(&app, session.id, turn, 25, flat_output);
    let invalid_call = ToolCallId::new();
    let mut invalid = wait_call(invalid_call, &ids);
    if let HistoryItemPayload::ToolCall { arguments_json, .. } = &mut invalid {
        *arguments_json = serde_json::json!({"job_ids":ids[0]}).to_string();
    }
    history(&app, session.id, turn, 26, invalid);
    history(
        &app,
        session.id,
        turn,
        27,
        tool_output(invalid_call, "UNRELATED_INVALID_ARGUMENTS"),
    );
    let service = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let detail = service
        .history_detail(McpHistoryDirection::Instruction, &target.id.to_string())
        .await
        .unwrap();
    assert!(detail.truncated);
    assert!(!detail.row.result_received);
    assert!(detail.markdown.contains("本文は省略または形式不一致"));
    assert!(detail.markdown.contains("\"result_body_recorded\": false"));
    assert!(detail.markdown.contains("\"state\": \"completed\""));
    assert!(detail.markdown.contains("\"state\": \"awaiting_approval\""));
    assert!(detail.markdown.contains(&truncated_call.to_string()));
    assert!(detail.markdown.contains(&flat_call.to_string()));
    assert!(!detail.markdown.contains("UNRELATED_"));
    assert!(!detail.markdown.contains(&invalid_call.to_string()));
    assert_eq!(detail.row.updated_at_ms, Some(1025));
}

#[tokio::test]
async fn instruction_history_is_persisted_offline_exact_peer_and_turn_scoped() {
    let (_temp, app, draft) = fixture().await;
    let session = app
        .store
        .session_repo()
        .create_session(draft.clone())
        .await
        .unwrap();
    let turn = TurnId::new();
    let mut target = reference(session.id, turn, "wanted");
    let mut sibling = reference(session.id, turn, "sibling");
    sibling.device_id = "device-c".into();
    sibling.peer.device_id = "device-c".into();
    sibling.peer.label = "WinC".into();
    let wanted_call = ToolCallId::new();
    let sibling_call = ToolCallId::new();
    let same_peer_other_call = ToolCallId::new();
    history(
        &app,
        session.id,
        turn,
        0,
        user("WinB の CPU を調べてください。"),
    );
    history(
        &app,
        session.id,
        turn,
        1,
        tool_call(&target, "wanted", wanted_call, "CPU調査指示"),
    );
    history(
        &app,
        session.id,
        turn,
        2,
        tool_call(&sibling, "sibling", sibling_call, "UNRELATED_WIN_C_PROMPT"),
    );
    history(
        &app,
        session.id,
        turn,
        3,
        tool_call(
            &target,
            "different-request",
            same_peer_other_call,
            "UNRELATED_SAME_PEER_PROMPT",
        ),
    );
    history(
        &app,
        session.id,
        turn,
        4,
        tool_output(wanted_call, "WinB CPU: 12.5%"),
    );
    history(
        &app,
        session.id,
        turn,
        5,
        tool_output(sibling_call, "UNRELATED_WIN_C_OUTPUT"),
    );
    history(
        &app,
        session.id,
        turn,
        6,
        tool_output(same_peer_other_call, "UNRELATED_SAME_PEER_OUTPUT"),
    );
    history(
        &app,
        session.id,
        turn,
        7,
        HistoryItemPayload::ToolCall {
            call_id: ToolCallId::new(),
            response_id: ModelResponseId::new(),
            model_call_id: "local".into(),
            tool_name: "shell".into(),
            arguments_json: "{\"command\":\"UNRELATED_LOCAL_SHELL\"}".into(),
        },
    );
    history(
        &app,
        session.id,
        TurnId::new(),
        0,
        user("UNRELATED_LATER_TURN"),
    );
    target.state = "completed".into();
    target.result = Some("WinB CPU: 12.5% — 測定済み".into());
    app.store
        .remote_job_store()
        .accept_device_reference(&target)
        .unwrap();
    app.store
        .remote_job_store()
        .accept_device_reference(&sibling)
        .unwrap();
    let reopened = SqliteStore::open(app.store.paths()).unwrap();
    reopened.migrate().unwrap();
    let restored = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
        &draft.cwd,
        StoreBundle::new(reopened),
        ResolvedConfig::default(),
    )
    .await
    .unwrap();
    let service = RemoteJobService::new(restored.process_runtime.clone()).unwrap();
    let detail = service
        .history_detail(McpHistoryDirection::Instruction, &target.id.to_string())
        .await
        .unwrap();
    assert_eq!(detail.row.state_source, "last_observed");
    assert_eq!(detail.row.state, "completed");
    assert!(detail.row.result_received);
    assert!(!detail.row.can_stop);
    assert_eq!(detail.row.updated_at_ms, Some(1004));
    assert!(detail.markdown.contains("CPU調査指示"));
    assert!(detail.markdown.contains("12.5%"));
    for private in [
        "UNRELATED_WIN_C",
        "UNRELATED_SAME_PEER",
        "UNRELATED_LOCAL_SHELL",
        "UNRELATED_LATER_TURN",
        "SECRET_MUST_NOT_EXPORT",
        "CERTIFICATE_MUST_NOT_EXPORT",
        "METADATA_MUST_NOT_EXPORT",
    ] {
        assert!(!detail.markdown.contains(private), "leaked {private}");
    }
    assert!(!detail.truncated);
    assert!(
        service
            .history_detail(McpHistoryDirection::Execution, &target.id.to_string())
            .await
            .is_err()
    );
    assert!(
        service
            .history_detail(McpHistoryDirection::Instruction, "../../db")
            .await
            .is_err()
    );
    assert!(serde_json::from_str::<McpHistoryDirection>("\"sideways\"").is_err());
}

#[tokio::test]
async fn instruction_paging_reaches_older_than_64_and_retains_anchor_when_new_work_arrives() {
    let (_temp, app, draft) = fixture().await;
    let session = app
        .store
        .session_repo()
        .create_session(draft)
        .await
        .unwrap();
    let turn = TurnId::new();
    let mut expected = HashSet::new();
    for index in 0..75 {
        let row = reference(session.id, turn, &format!("job-{index}"));
        expected.insert(row.id.to_string());
        app.store
            .remote_job_store()
            .accept_device_reference(&row)
            .unwrap();
    }
    let service = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let mut page = service
        .history_page(McpHistoryDirection::Instruction, 0, 20, None)
        .await
        .unwrap();
    let anchor = page.anchor.clone().unwrap();
    let latest = reference(session.id, turn, "new-arrival");
    app.store
        .remote_job_store()
        .accept_device_reference(&latest)
        .unwrap();
    let mut seen = HashSet::new();
    loop {
        for row in &page.rows {
            assert!(seen.insert(row.id.clone()), "duplicate");
            assert_eq!(row.state, "running");
            assert_eq!(row.state_source, "last_observed");
            assert_eq!(row.updated_at_ms, None);
        }
        let Some(offset) = page.next_offset else {
            break;
        };
        page = service
            .history_page(McpHistoryDirection::Instruction, offset, 20, Some(&anchor))
            .await
            .unwrap();
    }
    assert_eq!(seen, expected);
    assert!(!seen.contains(&latest.id.to_string()));
    assert!(
        service
            .history_page(McpHistoryDirection::Execution, 0, 20, Some(&anchor))
            .await
            .is_err(),
        "a cursor from the other direction is not reusable"
    );
    assert!(
        service
            .history_page(McpHistoryDirection::Instruction, 0, 20, Some("bad-anchor"))
            .await
            .is_err()
    );
    assert!(
        service
            .history_page(McpHistoryDirection::Instruction, 0, 100, None)
            .await
            .unwrap()
            .rows
            .iter()
            .any(|row| row.id == latest.id.to_string())
    );
    assert!(
        service
            .history_page(McpHistoryDirection::Execution, 0, 100, None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
}

#[tokio::test]
async fn execution_history_includes_direct_and_hub_receipts_with_receiver_off_and_exact_admitted_turn()
 {
    let (_temp, app, draft) = fixture().await;
    let request = RemoteTaskRequest {
        request_key: "receiver-request".into(),
        parent: RemoteJobParent {
            peer_id: "WinA".into(),
            task_id: "root-task".into(),
            turn_id: "parent-turn".into(),
        },
        prompt: "CPU調査を実行".into(),
        inputs: vec![],
    };
    let (job, _) = app
        .store
        .remote_job_store()
        .accept(
            "principal",
            Ulid::new(),
            &request,
            "{\"target\":{\"kind\":\"temp\"}}",
            &draft,
        )
        .unwrap();
    let turn = TurnId::new();
    let admission = app
        .store
        .session_repo()
        .admit_remote_task(
            job.session_id,
            turn,
            job.id,
            &UserTurn {
                turn_id: turn,
                items: vec![UserInputItem::Text {
                    text: "CPU調査を実行".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    history(
        &app,
        job.session_id,
        turn,
        20,
        HistoryItemPayload::Error {
            message: "shell executable unavailable: fixture".into(),
        },
    );
    history(
        &app,
        job.session_id,
        TurnId::new(),
        0,
        user("UNRELATED_RECEIVER_TURN"),
    );
    app.store
        .protocol_event_store()
        .seed_turn_item_for_test(&TurnItem {
            id: TurnItemId::new(),
            session_id: job.session_id,
            turn_id: turn,
            source_item_id: None,
            sequence_no: 40,
            payload: TurnItemPayload::Warning {
                message: "fixture failure".into(),
            },
        })
        .unwrap();
    // No dispatcher or receiver is started. Inspecting durable receipts cannot execute them.
    let service = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let page = service
        .history_page(McpHistoryDirection::Execution, 0, 20, None)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].id, job.id.to_string());
    assert_eq!(page.rows[0].target_label, "temp");
    assert!(!page.rows[0].can_stop);
    assert_eq!(
        page.rows[0].state, "unknown",
        "absence of a worker is not durable proof of cancellation"
    );
    let detail = service
        .history_detail(McpHistoryDirection::Execution, &job.id.to_string())
        .await
        .unwrap();
    assert!(detail.markdown.contains("CPU調査を実行"));
    assert!(detail.markdown.contains("shell executable unavailable"));
    assert!(detail.markdown.contains("fixture failure"));
    assert!(!detail.markdown.contains("UNRELATED_RECEIVER_TURN"));
    assert!(!detail.row.result_received);
    assert!(
        service
            .history_detail(McpHistoryDirection::Instruction, &job.id.to_string())
            .await
            .is_err()
    );
    assert!(!service.has_active_jobs());

    // Use the same atomic terminal/session boundary as execution. Seeding only
    // a terminal event would leave a Running session with a settled active turn.
    app.store
        .session_repo()
        .terminalize_admitted_turn_with_protocol_event(
            job.session_id,
            admission.admission_id,
            &crate::session::RunEvent::TurnTerminal {
                session_id: job.session_id,
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Failed {
                        error: "fixture failure".into(),
                    },
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                }),
            },
            turn,
            None,
            None,
        )
        .await
        .unwrap();
    let settled = service
        .history_detail(McpHistoryDirection::Execution, &job.id.to_string())
        .await
        .unwrap();
    assert_eq!(settled.row.state, "failed");
    assert!(
        !settled.row.result_received,
        "receiver completion is not sender receipt"
    );

    let profile = Ulid::new();
    let claims = crate::device_network::GrantClaims {
        hub_id: "hub".into(),
        origin_device_id: "Win00".into(),
        actor_device_id: "Win19".into(),
        audience_device_id: "Win20".into(),
        profile_id: profile.to_string(),
        mode: "agent".into(),
        scope_id: "scope".into(),
        root_task_id: "hub-root-task".into(),
        request_key: "hub-receiver-request".into(),
        parent_job_id: Some("parent-job".into()),
        depth: 1,
        device_path: vec!["Win00".into(), "Win19".into(), "Win20".into()],
    };
    let mut hub_request = request.clone();
    hub_request.request_key = claims.request_key.clone();
    hub_request.prompt = "Hub経由の実行指示".into();
    let scope = serde_json::json!({"receiver":"{\"target\":{\"kind\":\"temp\"},\"base_url\":\"http://SECRET_ENDPOINT\"}","network":claims}).to_string();
    let (hub_job, _) = app
        .store
        .remote_job_store()
        .accept_with_network(
            "hub-principal",
            profile,
            &hub_request,
            &scope,
            &draft,
            Some("receipt-grant"),
        )
        .unwrap();
    let reopened = SqliteStore::open(app.store.paths()).unwrap();
    reopened.migrate().unwrap();
    let restored = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
        &draft.cwd,
        StoreBundle::new(reopened),
        ResolvedConfig::default(),
    )
    .await
    .unwrap();
    let service = RemoteJobService::new(restored.process_runtime.clone()).unwrap();
    let detail = service
        .history_detail(McpHistoryDirection::Execution, &hub_job.id.to_string())
        .await
        .unwrap();
    assert_eq!(detail.row.peer_label, "Win19");
    assert_eq!(detail.row.root_task_id, "hub-root-task");
    assert_eq!(detail.row.device_path, vec!["Win00", "Win19", "Win20"]);
    assert_eq!(detail.row.target_label, "temp");
    assert_eq!(detail.row.state, "unknown");
    assert!(detail.markdown.contains("Hub経由の実行指示"));
    assert!(!detail.markdown.contains("SECRET_ENDPOINT"));
    assert!(!detail.markdown.contains("receipt-grant"));
    assert!(!service.has_active_jobs());
}

#[tokio::test]
async fn history_export_bounds_nonascii_large_items_and_preserves_latest_error() {
    let (_temp, app, draft) = fixture().await;
    let request = RemoteTaskRequest {
        request_key: "large-request".into(),
        parent: RemoteJobParent {
            peer_id: "WinA".into(),
            task_id: "root".into(),
            turn_id: "parent".into(),
        },
        prompt: "日本語の長い調査".into(),
        inputs: vec![],
    };
    let (job, _) = app
        .store
        .remote_job_store()
        .accept("principal", Ulid::new(), &request, "{}", &draft)
        .unwrap();
    let turn = TurnId::new();
    app.store
        .session_repo()
        .admit_remote_task(
            job.session_id,
            turn,
            job.id,
            &UserTurn {
                turn_id: turn,
                items: vec![UserInputItem::Text {
                    text: "日本語の長い調査".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    for index in 10..300 {
        history(
            &app,
            job.session_id,
            turn,
            index,
            HistoryItemPayload::Error {
                message: format!("記録 {index}"),
            },
        );
    }
    history(
        &app,
        job.session_id,
        turn,
        300,
        HistoryItemPayload::Error {
            message: "大".repeat(MAX_ITEM_BYTES),
        },
    );
    history(
        &app,
        job.session_id,
        turn,
        301,
        HistoryItemPayload::Error {
            message: "最後のエラー — 429".into(),
        },
    );
    let service = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let detail = service
        .history_detail(McpHistoryDirection::Execution, &job.id.to_string())
        .await
        .unwrap();
    assert!(detail.truncated);
    assert!(detail.markdown.contains("省略あり"));
    assert!(detail.markdown.contains("日本語の長い調査"));
    assert!(detail.markdown.contains("最後のエラー — 429"));
    assert!(detail.markdown.len() <= MAX_MARKDOWN_BYTES + 2048);
    let path = draft.cwd.join("MCP履歴.md");
    std::fs::write(&path, &detail.markdown).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), detail.markdown);
    let mut output = String::new();
    let mut clipped = false;
    append_block(
        &mut output,
        "untrusted delimiters",
        &"`".repeat(MAX_MARKDOWN_BYTES * 2),
        &mut clipped,
    );
    assert!(clipped);
    assert!(output.len() <= MAX_MARKDOWN_BYTES);
}

#[tokio::test]
async fn hub_recent_history_keeps_latest_bodies_under_both_read_and_wire_budgets() {
    let (_temp, app, draft) = fixture().await;
    let request = RemoteTaskRequest {
        request_key: "recent-history".into(),
        parent: RemoteJobParent {
            peer_id: "WinA".into(),
            task_id: "root".into(),
            turn_id: "parent".into(),
        },
        prompt: "調査対象の概要".into(),
        inputs: vec![],
    };
    let (job, _) = app
        .store
        .remote_job_store()
        .accept("principal", Ulid::new(), &request, "{}", &draft)
        .unwrap();
    let turn = TurnId::new();
    app.store
        .session_repo()
        .admit_remote_task(
            job.session_id,
            turn,
            job.id,
            &UserTurn {
                turn_id: turn,
                items: vec![UserInputItem::Text {
                    text: "ORIGINAL_USER_REQUEST".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    // The old ascending read exhausts its 2 MiB before reaching the latest records.
    for sequence in 10..90 {
        history(
            &app,
            job.session_id,
            turn,
            sequence,
            HistoryItemPayload::Error {
                message: format!("OLD_{sequence}\n{}", "過去の出力😀\n".repeat(2000)),
            },
        );
    }
    let call = ToolCallId::new();
    history(
        &app,
        job.session_id,
        turn,
        1000,
        HistoryItemPayload::ToolCall {
            call_id: call,
            response_id: ModelResponseId::new(),
            model_call_id: "fixture".into(),
            tool_name: "shell".into(),
            arguments_json: json!({"command":"read observed result","api_key":"PRIVATE_ARGUMENT"})
                .to_string(),
        },
    );
    history(
        &app,
        job.session_id,
        turn,
        1001,
        tool_output(
            call,
            &format!(
                "LATEST_OUTPUT_START\n{}\nLATEST_OUTPUT_ERROR_END",
                "日本語😀\n".repeat(10000)
            ),
        ),
    );
    history(
        &app,
        job.session_id,
        turn,
        1002,
        HistoryItemPayload::Error {
            message: "大".repeat(MAX_ITEM_BYTES),
        },
    );
    history(
        &app,
        job.session_id,
        turn,
        1003,
        HistoryItemPayload::Error {
            message: "LATEST_AFTER_OVERSIZED".into(),
        },
    );
    history(
        &app,
        job.session_id,
        TurnId::new(),
        2000,
        HistoryItemPayload::Error {
            message: "UNRELATED_TURN_MUST_NOT_EXPORT".into(),
        },
    );
    app.store
        .protocol_event_store()
        .seed_turn_item_for_test(&TurnItem {
            id: TurnItemId::new(),
            session_id: job.session_id,
            turn_id: turn,
            source_item_id: None,
            sequence_no: 2000,
            payload: TurnItemPayload::Error {
                message: "LATEST_PROGRESS_ERROR".into(),
            },
        })
        .unwrap();
    let service = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let local_before = service
        .history_detail(McpHistoryDirection::Execution, &job.id.to_string())
        .await
        .unwrap();
    let detail = service
        .history_detail_for_hub(McpHistoryDirection::Execution, &job.id.to_string())
        .await
        .unwrap();
    for expected in [
        "調査対象の概要",
        "記録 1003 / Unix ms 2003",
        "進行記録: 2000",
        "LATEST_AFTER_OVERSIZED",
        "LATEST_OUTPUT_START",
        "LATEST_OUTPUT_ERROR_END",
        "LATEST_PROGRESS_ERROR",
        "省略あり",
        "本文の途中を省略",
    ] {
        assert!(detail.markdown.contains(expected), "missing {expected}");
    }
    for private in [
        "PRIVATE_ARGUMENT",
        "METADATA_MUST_NOT_EXPORT",
        "UNRELATED_TURN_MUST_NOT_EXPORT",
    ] {
        assert!(!detail.markdown.contains(private), "leaked {private}");
    }
    assert!(detail.truncated);
    assert!(detail.markdown.len() <= HUB_MARKDOWN_BYTES);
    assert!(serde_json::to_vec(&detail).unwrap().len() < 512 * 1024);
    assert!(detail.row.updated_at_ms.is_some());
    let local_after = service
        .history_detail(McpHistoryDirection::Execution, &job.id.to_string())
        .await
        .unwrap();
    assert_eq!(
        local_before.markdown, local_after.markdown,
        "Hub projection must not alter the local export"
    );
    assert!(local_after.markdown.contains("ORIGINAL_USER_REQUEST"));
    // History and progress share the turn's sequence allocator; progress claimed 2000.
    history(
        &app,
        job.session_id,
        turn,
        2001,
        HistoryItemPayload::Error {
            message: "NEWLY_APPENDED_RECORD".into(),
        },
    );
    let refreshed = service
        .history_detail_for_hub(McpHistoryDirection::Execution, &job.id.to_string())
        .await
        .unwrap();
    assert!(
        refreshed.markdown.contains("記録 2001 / Unix ms 3001"),
        "refreshed cutoff missing: {}",
        refreshed.markdown
    );
    assert!(refreshed.markdown.contains("NEWLY_APPENDED_RECORD"));
    assert!(
        !detail.markdown.contains("NEWLY_APPENDED_RECORD"),
        "the old snapshot stays immutable"
    );
}

#[tokio::test]
async fn hub_recent_instruction_history_isolates_siblings_and_later_wait_results() {
    let (_temp, app, draft) = fixture().await;
    let session = app
        .store
        .session_repo()
        .create_session(draft)
        .await
        .unwrap();
    let turn = TurnId::new();
    let later = TurnId::new();
    let target = reference(session.id, turn, "target-recent");
    let sibling = reference(session.id, turn, "sibling-recent");
    app.store
        .remote_job_store()
        .accept_device_reference(&target)
        .unwrap();
    app.store
        .remote_job_store()
        .accept_device_reference(&sibling)
        .unwrap();
    let wanted_call = ToolCallId::new();
    let sibling_call = ToolCallId::new();
    history(&app, session.id, turn, 0, user("元の依頼"));
    history(
        &app,
        session.id,
        turn,
        1,
        tool_call(&target, "target-recent", wanted_call, "TARGET_CALL"),
    );
    history(
        &app,
        session.id,
        turn,
        2,
        tool_output(wanted_call, "TARGET_RESPONSE"),
    );
    history(
        &app,
        session.id,
        turn,
        3,
        tool_call(
            &sibling,
            "sibling-recent",
            sibling_call,
            "PRIVATE_SIBLING_CALL",
        ),
    );
    history(
        &app,
        session.id,
        turn,
        4,
        tool_output(sibling_call, "PRIVATE_SIBLING_RESPONSE"),
    );
    let wait = ToolCallId::new();
    let ids = vec![
        target.job_id.clone().unwrap(),
        sibling.job_id.clone().unwrap(),
    ];
    history(&app, session.id, later, 10, wait_call(wait, &ids));
    history(&app, session.id, later, 11, tool_output(wait, &json!({
        "interrupted":false,"timed_out":false,"requested_job_ids":ids,
        "jobs":[wait_job(&target,"LATEST_TARGET_WAIT"),wait_job(&sibling,"PRIVATE_SIBLING_WAIT")]
    }).to_string()));
    history(&app, session.id, later, 12, user("PRIVATE_LATER_USER"));
    let service = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let detail = service
        .history_detail_for_hub(McpHistoryDirection::Instruction, &target.id.to_string())
        .await
        .unwrap();
    for expected in [
        "TARGET_CALL",
        "TARGET_RESPONSE",
        "LATEST_TARGET_WAIT",
        "待機記録 11",
        &later.to_string(),
    ] {
        assert!(detail.markdown.contains(expected), "missing {expected}");
    }
    for private in [
        "PRIVATE_",
        "SECRET_MUST_NOT_EXPORT",
        "CERTIFICATE_MUST_NOT_EXPORT",
        "METADATA_MUST_NOT_EXPORT",
        sibling.job_id.as_deref().unwrap(),
    ] {
        assert!(!detail.markdown.contains(private), "leaked {private}");
    }
    assert_eq!(detail.row.updated_at_ms, Some(1011));
    assert!(!detail.row.result_received);
    assert!(!detail.truncated);
    assert!(detail.markdown.len() <= HUB_MARKDOWN_BYTES);
    assert!(
        service
            .history_detail_for_hub(McpHistoryDirection::Instruction, &Ulid::new().to_string())
            .await
            .is_err()
    );
    assert!(
        service
            .history_detail_for_hub(McpHistoryDirection::Execution, &target.id.to_string())
            .await
            .is_err()
    );
}
