use super::*;
use crate::device_network::{DeviceNetworkService, DirectoryPeer};
use crate::remote_agent::store::StoredDeviceReference;
use serde_json::json;

#[tokio::test]
async fn agent_remote_wait_keeps_model_idle_until_the_saved_job_result_arrives() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
    let paths = StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/history.sqlite3"),
        truncation_dir: root.join("data/truncation"),
    };
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.permissions.access_mode = AccessMode::Default;
    config.mcp = crate::config::McpConfig {
        enabled: true,
        servers: vec![crate::config::McpServerConfig {
            id: "hub-device-test-worker".into(),
            display_name: Some("Worker".into()),
            enabled: true,
            transport: crate::config::McpTransportKind::Http,
            base_url: "https://127.0.0.1:7332/mcp".into(),
            timeout_ms: 3000,
            remote_agent: true,
            trusted_certificate_pem: None,
            headers: Default::default(),
            tool_routes: vec![],
        }],
    };
    // This runtime has no Hub configuration or started heartbeat. The wait
    // observes its isolated canonical store and cannot contact any endpoint.
    let network = DeviceNetworkService::for_workspace(
        root.join("device"),
        root.clone(),
        store.clone(),
        config.clone(),
    )
    .await
    .unwrap();
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).unwrap();
    store
        .project_repo()
        .upsert_project(workspace.project_id, &root, "remote wait test", "none")
        .await
        .unwrap();
    let sessions = crate::session::SessionService::new(store.clone());
    let session = sessions
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("Wait for delegated work".into()),
                cwd: root.clone(),
                model: "scripted".into(),
                base_url: "http://local".into(),
                access_mode: config.permissions.access_mode,
                provider_connection: Some(
                    crate::session::SessionProviderConnection::from_model_config(&config.model),
                ),
            },
            workspace,
        )
        .await
        .unwrap();
    let session_id = session.session.id;
    let turn_id = TurnId::new();
    let admission = store
        .session_repo()
        .admit_session_turn(session_id, turn_id)
        .await
        .unwrap()
        .unwrap();
    let user_turn = UserTurn {
        turn_id,
        items: vec![UserInputItem::Text {
            text: "委任した作業を待ち、届いた結果を報告してください。".into(),
        }],
        prompt_dispatch: None,
        editor_context: None,
    };
    sessions
        .store_user_turn_with_protocol_bundle(
            &session,
            admission.admission_id,
            &user_turn,
            turn_id,
            0,
        )
        .await
        .unwrap();
    let mut builder = context_manager::ContextManager::active_history_builder();
    let mut user_id = None;
    let initial = store
        .protocol_event_store()
        .visit_active_history_pages_for_session(
            session_id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |page| {
                user_id = user_id.or_else(|| {
                    page.items
                        .iter()
                        .find(|item| matches!(&item.payload, HistoryItemPayload::UserTurn { .. }))
                        .map(|item| item.id)
                });
                builder.ingest_page(page.items);
                Ok(())
            },
        )
        .unwrap();
    let context = builder.finish(initial.append_fence, initial.canonical_count);
    let job_id = ulid::Ulid::new().to_string();
    let peer = DirectoryPeer {
        device_id: "worker".into(),
        label: "Worker".into(),
        profile_id: "profile".into(),
        name: "temp".into(),
        endpoint: "https://127.0.0.1:7332/mcp".into(),
        mode: "agent".into(),
        scope_id: "scope".into(),
        certificate_pem: "unused test certificate".into(),
        certificate_sha256: "a".repeat(64),
    };
    let mut row = store
        .remote_job_store()
        .accept_device_reference(&StoredDeviceReference {
            id: ulid::Ulid::new(),
            session_id,
            turn_id,
            device_id: peer.device_id.clone(),
            profile_id: peer.profile_id.clone(),
            root_task_id: turn_id.to_string(),
            request_key: "existing-delegation".into(),
            prompt_hash: "a".repeat(64),
            parent_grant_id: None,
            parent_job_id: None,
            peer,
            claims: None,
            job_id: Some(job_id.clone()),
            state: "running".into(),
            stop_status: "none".into(),
            result: None,
        })
        .unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let llm = Arc::new(ScriptedClient {
        requests: requests.clone(),
        outcomes: Mutex::new(vec![
            ScriptedOutcome::Response(ScriptedResponse {
                events: vec![
                    LlmEvent::ToolCallStart {
                        call_id: "wait-one-job".into(),
                        tool_name: "wait_remote_tasks".into(),
                    },
                    LlmEvent::ToolCallArgsDelta {
                        call_id: "wait-one-job".into(),
                        delta: json!({"job_ids":[job_id],"timeout_ms":10000}).to_string(),
                    },
                ],
                finish_reason: FinishReason::ToolCall,
            }),
            ScriptedOutcome::Response(ScriptedResponse {
                events: vec![LlmEvent::TextDelta(
                    "Workerの完了結果を受け取りました。".into(),
                )],
                finish_reason: FinishReason::Stop,
            }),
        ]),
    });
    let services = test_tool_services(&config, &store, paths);
    let agent = AgentLoop::new(
        llm,
        ToolRegistry::builtin(services.clone()),
        store.clone(),
        PromptBuilder,
        services,
    );
    let mode = mode::CollaborationMode::resolve(mode::ModeKind::Default);
    let policy = crate::llm::model_policy::ResolvedTurnPolicy::resolve(
        &mode,
        crate::llm::model_policy::ModelPolicy::from_config(&config),
        crate::llm::model_policy::ProviderCapabilities::from_config(&config),
        config.model.reasoning_summary,
    )
    .unwrap();
    let run_control = RunControl::new();
    let active_run = store
        .active_runs()
        .try_start(session_id, run_control.clone())
        .unwrap();
    active_run
        .set_turn_target(turn_id, admission.admission_revision)
        .unwrap();
    let run_request = AgentRunRequest {
        session,
        turn: Arc::new(turn_context::TurnContext {
            turn_id,
            admission_id: admission.admission_id,
            mode,
            policy: Arc::new(policy),
            config: Arc::new(crate::config::ResolvedTurnConfig::capture(config).unwrap()),
            goal: None,
            current_time: crate::context::current_time::CurrentTimeSnapshot::now(),
        }),
        context,
        run_control,
        agent_context: None,
        initial_user_history_item_id: user_id,
    };
    let mut prompt = DecisionPrompt::default();
    let mut sink = CapturingSink {
        sequence_no: store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)
            .unwrap()
            .unwrap()
            .1,
        ..CapturingSink::default()
    };
    let summary = {
        let mut recording = crate::protocol::ProtocolRecordingSink::new(
            store.protocol_event_store(),
            Some(session_id),
            turn_id,
            &mut sink,
        )
        .with_admission_id(admission.admission_id);
        let run = agent.run(run_request, &mut prompt, &mut recording);
        tokio::pin!(run);
        for _ in 0..3 {
            store
                .remote_job_store()
                .update_device_reference(&row)
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(1100), &mut run)
                    .await
                    .is_err(),
                "the actual tool must stay pending on unchanged observations"
            );
            assert_eq!(
                requests.lock().unwrap().len(),
                1,
                "durable polling must not re-enter the provider"
            );
        }
        row.state = "completed".into();
        row.result = Some("remote-ready-result 日本語".into());
        store
            .remote_job_store()
            .update_device_reference(&row)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), &mut run)
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(summary.status(), SessionStatus::Completed);
    assert_eq!(summary.tool_call_count(), 1);
    assert_eq!(summary.failed_tool_count(), 0);
    assert_eq!(summary.metrics().model_request_count, 2);
    assert_canonical_tool_statuses(&store, session_id, &[ToolLifecycleStatus::Completed]);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(request_tool_names(&requests[0]).contains(&"wait_remote_tasks".into()));
    let output = requests[1]
        .messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::Tool { result, .. } => Some(result.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(output.len(), 1);
    let result: Value = serde_json::from_str(output[0]).unwrap();
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["jobs"][0]["job_id"], job_id);
    assert_eq!(result["jobs"][0]["state"], "completed");
    assert_eq!(result["jobs"][0]["result"], "remote-ready-result 日本語");
    drop(requests);
    assert!(
        prompt.requests.is_empty(),
        "readonly wait must not invoke Guardian or request approval"
    );
    drop(active_run);
    network.shutdown().await;
}
