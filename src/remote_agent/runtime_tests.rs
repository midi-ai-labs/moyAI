use super::*;
use crate::config::{AccessMode, ProviderProfile};
use crate::mcp_publish::PublishAuthentication;
use crate::session::ProjectRepository;
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use std::sync::atomic::{AtomicUsize, Ordering};
#[path = "artifact_runtime_tests.rs"]
mod artifact_runtime_tests;
#[path = "network_fixture.rs"]
mod network_fixture;

#[tokio::test]
async fn default_receiver_waits_for_local_approval_and_preserves_denial_and_stop() {
    for decision in [
        ReviewDecision::Approved,
        ReviewDecision::Denied,
        ReviewDecision::Abort,
    ] {
        let (_temp, mut app, profile) = fixture("http://127.0.0.1:1/v1").await;
        let outside = app
            .workspace
            .root
            .parent()
            .unwrap()
            .join("receiver-reviewed.txt");
        // PathGuard correctly rejects arbitrary external patch paths before review.
        // A shell operation with an explicit, justified elevation exercises human review.
        let command = if cfg!(windows) {
            format!(
                "[System.IO.File]::WriteAllText('{}', 'receiver approved content')",
                outside.as_str().replace('\'', "''")
            )
        } else {
            format!(
                "printf %s 'receiver approved content' > '{}'",
                outside.as_str().replace('\'', "'\\''")
            )
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let router = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move || {
            let command = command.clone(); let count = count.clone();
            async move {
                let (delta, finish) = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    (json!({"role":"assistant","tool_calls":[{"index":0,"id":"reviewed-edit","type":"function","function":{"name":"shell","arguments":json!({"command":command,"sandbox_permissions":"require_escalated","justification":"Create the explicit fixture file outside the receiver workspace only after receiver approval."}).to_string()}}]}), "tool_calls")
                } else { (json!({"role":"assistant","content":"Receiver request processed"}), "stop") };
                let chunk = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                let end = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
                ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        app.config.model.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
        let dispatcher = jobs
            .dispatcher(profile.clone(), app.config.clone(), vec![])
            .await
            .unwrap();
        let accepted = call(&dispatcher, "delegate_task", task("reviewed-request")).await;
        let id = accepted["job_id"].as_str().unwrap();
        let pending = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(pending) = jobs.pending_approval() {
                    return pending;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(pending.context.job_id.to_string(), id);
        assert_eq!(pending.context.profile_id, profile.id);
        let activity = jobs.activity_now();
        assert_eq!(activity.awaiting_approval, 1);
        assert_eq!(activity.running + activity.waiting + activity.cancelling, 0);
        assert_eq!(
            call(&dispatcher, "task_status", json!({"job_id":id})).await["state"],
            "awaiting_approval"
        );
        assert!(!outside.exists(), "permission precedes the actual write");
        assert!(!jobs.answer_approval(
            pending.confirmation_id,
            Ulid::new(),
            profile.id,
            ReviewDecision::Approved
        ));
        assert!(jobs.answer_approval(
            pending.confirmation_id,
            pending.context.job_id,
            profile.id,
            decision
        ));
        let finished = terminal(&dispatcher, id).await;
        if decision == ReviewDecision::Approved {
            assert_eq!(
                std::fs::read_to_string(&outside).unwrap(),
                "receiver approved content"
            );
        } else {
            assert!(!outside.exists());
        }
        assert_eq!(
            finished["state"],
            if decision == ReviewDecision::Abort {
                "interrupted"
            } else {
                "completed"
            },
            "{finished}"
        );
        assert!(!jobs.answer_approval(
            pending.confirmation_id,
            pending.context.job_id,
            profile.id,
            ReviewDecision::Approved
        ));
        assert!(jobs.drain_profile(profile.id, Duration::from_secs(3)).await);
        assert!(jobs.pending_approval().is_none());
        assert!(!jobs.activity_now().active());
        server.abort();
    }
}

async fn fixture(endpoint: &str) -> (tempfile::TempDir, App, PublishProfile) {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let paths = StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let mut config = ResolvedConfig::default();
    config.model.base_url = endpoint.into();
    config.model.model = "remote-fixture-model".into();
    config.model.provider_profile = ProviderProfile::OpenAiCompatible;
    config.model.max_retries = 0;
    let app = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
        &workspace,
        StoreBundle::new(sqlite),
        config,
    )
    .await
    .unwrap();
    let mut profile = PublishProfile::new(
        "Receiver fixture".into(),
        PublishTarget::Project {
            project_id: app.workspace.project_id,
            workspace_root: workspace,
        },
    );
    profile.mode = PublishMode::Agent {
        access_mode: AccessMode::Default,
    };
    profile.authentication = PublishAuthentication::LocalCredential {
        credential_id: Ulid::new(),
    };
    profile.enabled = true;
    (temp, app, profile)
}

fn task(key: &str) -> Value {
    json!({"request_key":key,"parent":{"peer_id":"WinA","task_id":"parent","turn_id":"turn"},"prompt":"受信端末の作業として短い回答を返してください。"})
}

async fn managed_fixture(
    app: &App,
    profile: &PublishProfile,
) -> (
    crate::device_network::DeviceNetworkService,
    RemoteJobService,
    Arc<dyn PublishToolDispatcher>,
    crate::device_network::VerifiedGrant,
    network_fixture::ReceiptPeer,
) {
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let path = app.store.paths().data_dir.parent().unwrap().join("config");
    let publish = crate::mcp_publish::PublishService::new(
        path.join("publish.json"),
        app.store.clone(),
        app.config.clone(),
    );
    let mut settings = crate::device_network::DeviceSettings::default();
    settings.hub_id = Some("hub".into());
    settings.device_id = Some("device-b".into());
    settings.label = if std::env::var("COMPUTERNAME").ok().as_deref() == Some("Win20-Worker") {
        "Hub-Receiver-Alias".into()
    } else {
        "Win20-Worker".into()
    };
    settings.receiver.profile_id = profile.id;
    let (shared, receipt_peer) =
        network_fixture::receipt_peer(&path.join("device"), &mut settings).await;
    crate::device_network::DeviceSettingsStore::new(path.join("device/device.json"))
        .save(&settings)
        .unwrap();
    let mut config = app.config.clone();
    config.device_network = shared;
    let network = crate::device_network::DeviceNetworkService::new(
        path.join("device"),
        app.store.clone(),
        config,
        jobs.clone(),
        publish,
    );
    assert_eq!(network.resume().await.unwrap().enrollment, "active");
    let target = network.projection_now();
    network
        .receiver(
            false,
            profile.target.clone(),
            AccessMode::Default,
            crate::hub::HubRouteMode::Direct,
            true,
            false,
            false,
            &target.revision,
            &target.generation,
        )
        .await
        .unwrap();
    let dispatcher = jobs
        .dispatcher_network(
            profile.clone(),
            app.config.clone(),
            vec![],
            network.downgrade(),
        )
        .await
        .unwrap();
    let authority = crate::device_network::VerifiedGrant {
        grant_id: "grant-one".into(),
        claims: crate::device_network::GrantClaims {
            hub_id: "hub".into(),
            origin_device_id: "device-a".into(),
            actor_device_id: "device-a".into(),
            audience_device_id: "device-b".into(),
            profile_id: profile.id.0.to_string(),
            mode: "agent".into(),
            scope_id: jobs.network_scope_id(profile.id).unwrap(),
            root_task_id: "root-one".into(),
            request_key: "request-one".into(),
            parent_job_id: None,
            depth: 1,
            device_path: vec!["device-a".into(), "device-b".into()],
        },
    };
    (network, jobs, dispatcher, authority, receipt_peer)
}

#[tokio::test]
async fn managed_job_uses_verified_identity_and_rotation_replays_the_same_job() {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (endpoint, count, _, server) = provider_recording(false, Some(requests.clone())).await;
    let (_temp, mut app, profile) = fixture(&endpoint).await;
    app.config.model.system_prompt = "Preserve the receiver's configured instructions.".into();
    let (network, jobs, dispatcher, authority, _receipt_peer) =
        managed_fixture(&app, &profile).await;
    let receiver = network.projection_now();
    assert_ne!(
        Some(receiver.display_name.as_str()),
        std::env::var("COMPUTERNAME").ok().as_deref()
    );
    assert!(
        dispatcher
            .call(
                "delegate_task",
                task("request-one"),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let mut other_receiver = authority.clone();
    other_receiver.claims.audience_device_id = "device-other".into();
    assert!(
        dispatcher
            .call_authorized(
                "delegate_task",
                task("request-one"),
                CancellationToken::new(),
                other_receiver
            )
            .await
            .is_err()
    );
    assert!(
        app.store
            .remote_job_store()
            .recent_all(64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let accepted = dispatcher
        .call_authorized(
            "delegate_task",
            task("request-one"),
            CancellationToken::new(),
            authority.clone(),
        )
        .await
        .unwrap()["structuredContent"]
        .clone();
    assert_eq!(accepted["parent"]["peer_id"], "device-a");
    assert_eq!(accepted["parent"]["task_id"], "root-one");
    let id = accepted["job_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let row = dispatcher
                .call_authorized(
                    "task_status",
                    json!({"job_id":id}),
                    CancellationToken::new(),
                    authority.clone(),
                )
                .await
                .unwrap();
            if row["structuredContent"]["state"] == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut rotated = authority.clone();
    rotated.grant_id = "renewed-grant".into();
    let replay = dispatcher
        .call_authorized(
            "delegate_task",
            task("request-one"),
            CancellationToken::new(),
            rotated,
        )
        .await
        .unwrap();
    assert_eq!(replay["structuredContent"]["job_id"], id);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let captured = requests.lock().unwrap();
    let first = captured.first().unwrap();
    let system = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "system")
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let identity = system
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["device_id"] == "device-b")
        .expect("model-visible receiver identity");
    assert_eq!(identity["display_name"], receiver.display_name);
    assert_eq!(
        identity["workspace_root"],
        app.workspace.authority_root().as_str()
    );
    assert_eq!(
        identity["os_hostname"],
        json!(std::env::var("COMPUTERNAME").ok())
    );
    assert!(system.contains("already running on the local receiving device"));
    assert!(system.contains("not a DNS hostname"));
    assert!(system.contains(&app.config.model.system_prompt));
    assert!(!system.contains("grant-one"));
    assert!(!system.contains("PRIVATE KEY"));
    assert!(
        first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "user"
                && message["content"] == task("request-one")["prompt"])
    );
    drop(captured);
    assert_eq!(
        app.config.model.system_prompt,
        "Preserve the receiver's configured instructions."
    );
    let stored = app.store.remote_job_store().recent_all(64).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].prompt_preview,
        task("request-one")["prompt"].as_str().unwrap()
    );
    assert!(stored[0].scope_json.contains("device-a"));
    assert!(!stored[0].scope_json.contains("grant-one"));
    assert!(!stored[0].scope_json.contains("renewed-grant"));
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(2)).await);
    server.abort();
}

#[tokio::test]
async fn managed_off_preserves_exact_job_control_and_rejects_other_lineage() {
    let (endpoint, _, release, server) = provider(true).await;
    let (_temp, app, profile) = fixture(&endpoint).await;
    let (_network, jobs, dispatcher, authority, _receipt_peer) =
        managed_fixture(&app, &profile).await;
    let accepted = dispatcher
        .call_authorized(
            "delegate_task",
            task("request-one"),
            CancellationToken::new(),
            authority.clone(),
        )
        .await
        .unwrap();
    let id = accepted["structuredContent"]["job_id"].as_str().unwrap();
    jobs.set_network_accepting(profile.id, false);
    assert!(
        dispatcher
            .call_authorized(
                "delegate_task",
                task("request-one"),
                CancellationToken::new(),
                authority.clone()
            )
            .await
            .is_err()
    );
    let mut other = authority.clone();
    other.claims.root_task_id = "other-root".into();
    assert!(
        dispatcher
            .call_authorized(
                "task_status",
                json!({"job_id":id}),
                CancellationToken::new(),
                other.clone()
            )
            .await
            .is_err()
    );
    assert!(
        dispatcher
            .call_authorized(
                "cancel_task",
                json!({"job_id":id}),
                CancellationToken::new(),
                other
            )
            .await
            .is_err()
    );
    let stopped = dispatcher
        .call_authorized(
            "cancel_task",
            json!({"job_id":id}),
            CancellationToken::new(),
            authority.clone(),
        )
        .await
        .unwrap();
    assert_ne!(stopped["structuredContent"]["state"], "completed");
    release.notify_waiters();
    assert!(
        jobs.drain_profile(profile.id, Duration::from_secs(10))
            .await
    );
    let final_row = dispatcher
        .call_authorized(
            "task_status",
            json!({"job_id":id}),
            CancellationToken::new(),
            authority,
        )
        .await
        .unwrap();
    assert_eq!(final_row["structuredContent"]["state"], "interrupted");
    server.abort();
}

async fn call(dispatcher: &Arc<dyn PublishToolDispatcher>, name: &str, args: Value) -> Value {
    dispatcher
        .call(name, args, CancellationToken::new())
        .await
        .unwrap()["structuredContent"]
        .clone()
}

async fn terminal(dispatcher: &Arc<dyn PublishToolDispatcher>, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let row = call(dispatcher, "task_status", json!({"job_id":id})).await;
            if matches!(
                row["state"].as_str(),
                Some("completed" | "failed" | "interrupted")
            ) {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn provider(
    block: bool,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    provider_recording(block, None).await
}

async fn provider_recording(
    block: bool,
    recorded: Option<Arc<Mutex<Vec<Value>>>>,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let count = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(tokio::sync::Notify::new());
    let calls = count.clone();
    let wait = released.clone();
    let app = axum::Router::new()
        .route("/v1/models", axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"remote-fixture-model"}]})) }))
        .route("/v1/chat/completions", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let calls = calls.clone(); let wait = wait.clone();
            let recorded = recorded.clone();
            async move {
                if let Some(recorded) = recorded { recorded.lock().unwrap().push(body.clone()); }
                let attempt = calls.fetch_add(1, Ordering::SeqCst);
                if block && attempt == 0 { wait.notified().await; }
                let (delta, finish) = if attempt == 0 {
                    (json!({"role":"assistant","tool_calls":[{"index":0,"id":"clock-call","type":"function","function":{"name":"current_time","arguments":"{}"}}]}), "tool_calls")
                } else {
                    assert!(body["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool" && message["content"].as_str().is_some_and(|text| text.contains("unix_ms"))), "the receiver must execute the real current_time tool before its final response");
                    (json!({"role":"assistant","content":"WinB receiver result"}), "stop")
                };
                let chunk = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                let end = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
                ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, count, released, server)
}

#[tokio::test]
async fn remote_job_runs_real_runservice_and_replay_never_reexecutes() {
    let (endpoint, count, _, server) = provider(false).await;
    let (_temp, app, profile) = fixture(&endpoint).await;
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    assert_eq!(
        jobs.activity_now(),
        super::super::RemoteActivityProjection::default()
    );
    let dispatcher = jobs
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    let accepted = call(&dispatcher, "delegate_task", task("one")).await;
    let id = accepted["job_id"].as_str().unwrap();
    let finished = terminal(&dispatcher, id).await;
    assert_eq!(finished["state"], "completed", "{finished}");
    assert_eq!(finished["result"], "WinB receiver result");
    assert!(!finished["can_stop"].as_bool().unwrap());
    let replay = call(&dispatcher, "delegate_task", task("one")).await;
    assert_eq!(replay["job_id"], accepted["job_id"]);
    assert_eq!(
        call(&dispatcher, "task_status", json!({"request_key":"one"})).await["job_id"],
        accepted["job_id"]
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let stored = app.store.remote_job_store().recent_all(64).unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].admitted_turn_id.is_some());
    assert!(
        app.store
            .session_repo()
            .durable_terminal_for_turn(stored[0].session_id, stored[0].admitted_turn_id.unwrap())
            .await
            .unwrap()
            .is_some()
    );
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(2)).await);
    assert!(
        !jobs.activity_now().active(),
        "terminal work no longer animates"
    );
    let delete_error = app
        .store
        .project_repo()
        .delete_project(app.workspace.project_id)
        .await
        .unwrap_err();
    assert!(delete_error.to_string().contains("遠隔タスクの実行記録"));
    assert_eq!(
        call(&dispatcher, "task_status", json!({"job_id":id})).await["result"],
        finished["result"],
        "a rejected project deletion preserves the completed result and receipt"
    );
    let mut other_profile = profile.clone();
    other_profile.id = PublishProfileId(Ulid::new());
    let other = jobs
        .dispatcher(other_profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    assert!(
        other
            .call(
                "task_status",
                json!({"job_id":id}),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(
        other
            .call(
                "cancel_task",
                json!({"job_id":id}),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    jobs.cancel_profile(profile.id);
    jobs.cancel_profile(other_profile.id);
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(2)).await);
    server.abort();
}

#[tokio::test]
async fn remote_job_cancel_owns_running_work_and_stopped_profile_cannot_accept() {
    let (endpoint, count, released, server) = provider(true).await;
    let (_temp, app, profile) = fixture(&endpoint).await;
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let dispatcher = jobs
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    let accepted = call(&dispatcher, "delegate_task", task("blocked")).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while count.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(jobs.activity_now().running, 1);
    assert!(
        dispatcher
            .call("delegate_task", task("second"), CancellationToken::new())
            .await
            .is_err()
    );
    let id = accepted["job_id"].as_str().unwrap();
    let _ = call(&dispatcher, "cancel_task", json!({"job_id":id})).await;
    let finished = terminal(&dispatcher, id).await;
    assert_eq!(finished["state"], "interrupted");
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(2)).await);
    assert!(
        !jobs.activity_now().active(),
        "cancelled work no longer animates"
    );
    jobs.cancel_profile(profile.id);
    assert!(
        dispatcher
            .call(
                "delegate_task",
                task("after-stop"),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    released.notify_waiters();
    server.abort();
}

#[tokio::test]
async fn completed_remote_server_survives_workspace_rebuild_and_profile_stop_drains_it() {
    let requests = Arc::new(AtomicUsize::new(0));
    let recorded = Arc::new(Mutex::new(Vec::<Value>::new()));
    let count = requests.clone();
    let captured = recorded.clone();
    let router = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(|| async {
                axum::Json(json!({"data":[{"id":"remote-fixture-model"}]}))
            }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let count = count.clone();
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(body);
                    let (delta, finish) = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        let command = if cfg!(windows) {
                            "Start-Sleep -Seconds 120"
                        } else {
                            "sleep 120"
                        };
                        (
                            json!({"role":"assistant","tool_calls":[{"index":0,"id":"start-server","type":"function","function":{"name":"shell_start","arguments":json!({"command":command,"timeout_ms":120_000}).to_string()}}]}),
                            "tool_calls",
                        )
                    } else {
                        (
                            json!({"role":"assistant","content":"Managed receiver command started"}),
                            "stop",
                        )
                    };
                    let chunk = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                    let end = json!({"id":"fixture","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (_temp, app, mut profile) = fixture(&endpoint).await;
    profile.mode = PublishMode::Agent {
        access_mode: AccessMode::FullAccess,
    };
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let dispatcher = jobs
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    let accepted = call(&dispatcher, "delegate_task", task("managed-server")).await;
    let id = accepted["job_id"].as_str().unwrap();
    let finished = terminal(&dispatcher, id).await;
    assert_eq!(finished["state"], "completed", "{finished}");
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let start_result = recorded
        .lock()
        .unwrap()
        .last()
        .and_then(|body| body["messages"].as_array())
        .and_then(|messages| {
            messages.iter().find(|message| {
                message["role"] == "tool" && message["tool_call_id"] == "start-server"
            })
        })
        .and_then(|message| message["content"].as_str())
        .map(str::to_owned)
        .expect("the receiver reports the real shell_start result to its provider");
    let start_result: Value = serde_json::from_str(&start_result).unwrap();
    assert_eq!(start_result["state"], "running", "{start_result}");
    assert!(start_result["pid"].as_u64().is_some_and(|pid| pid > 0));
    assert!(
        app.process_runtime
            .managed_shells()
            .has_profile_work(profile.id.0),
        "completion of the delegate job must leave its managed command alive"
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while jobs.activity_now().active() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !jobs
            .drain_profile(profile.id, Duration::from_millis(25))
            .await,
        "a terminal job must not make an active server disappear from profile drain"
    );

    let other_workspace = app.workspace.root.parent().unwrap().join("other-project");
    std::fs::create_dir_all(&other_workspace).unwrap();
    let rebuilt =
        AppBootstrap::rebuild_for_directory_as_workspace_root_with_process_runtime_and_config(
            &other_workspace,
            app.process_runtime.clone(),
            app.config.clone(),
        )
        .await
        .unwrap();
    assert_ne!(app.workspace.project_id, rebuilt.workspace.project_id);
    drop(app);
    assert!(
        rebuilt
            .process_runtime
            .managed_shells()
            .has_profile_work(profile.id.0),
        "workspace navigation shares the process owner even after the previous App is dropped"
    );

    jobs.cancel_profile(profile.id);
    assert!(
        jobs.drain_profile(profile.id, Duration::from_secs(15))
            .await
    );
    assert!(
        !rebuilt
            .process_runtime
            .managed_shells()
            .has_profile_work(profile.id.0)
    );
    let stored = rebuilt
        .store
        .remote_job_store()
        .get_for_profile(profile.id.0, id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        rebuilt
            .session_service
            .get_session(stored.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Completed,
        "stopping the managed command does not rewrite the completed job's terminal"
    );
    rebuilt.shutdown_managed_shells().await;
    server.abort();
}

#[tokio::test]
async fn remote_temp_has_its_own_workspace_and_rejects_ambiguous_status() {
    let (_temp, app, mut profile) = fixture("http://127.0.0.1:9/v1").await;
    profile.target = PublishTarget::Temp {};
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let dispatcher = jobs
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    {
        let state = jobs.inner.state.lock().unwrap();
        let scope = state.profiles.get(&profile.id).unwrap();
        assert_ne!(scope.app.workspace.root, app.workspace.root);
        assert!(!scope.app.config.multi_agent.enabled);
        assert!(!scope.app.config.mcp.enabled);
        assert!(
            scope
                .app
                .config
                .permissions
                .additional_read_roots
                .is_empty()
        );
    }
    let visible = app.store.project_repo().list_projects(64).await.unwrap();
    assert_eq!(
        visible.len(),
        1,
        "even a temp scope without jobs stays out of project discovery"
    );
    assert_eq!(visible[0].id, app.workspace.project_id);
    assert!(
        dispatcher
            .call(
                "task_status",
                json!({"job_id":Ulid::new(),"request_key":"both"}),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    jobs.cancel_profile(profile.id);
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(1)).await);
}

#[tokio::test]
async fn remote_pre_admission_stop_and_replacement_runtime_never_execute_the_receipt() {
    let (endpoint, count, _, server) = provider(false).await;
    let (_temp, app, profile) = fixture(&endpoint).await;
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let dispatcher = jobs
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    let id = profile.id;
    // Both actions run within one LocalSet operation. spawn_local cannot poll the
    // accepted job until this operation yields; the scope is sealed first.
    let accepted = jobs
        .submit(move |service| async move {
            let scope = service
                .inner
                .state
                .lock()
                .unwrap()
                .profiles
                .get(&id)
                .unwrap()
                .clone();
            let row = service
                .start_job(
                    &scope,
                    serde_json::from_value(task("before-start")).unwrap(),
                    None,
                    CancellationToken::new(),
                )
                .await?;
            service.cancel_profile(id);
            Ok(row)
        })
        .await
        .unwrap();
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(2)).await);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let stored = app
        .store
        .remote_job_store()
        .get(&profile.id.0.to_string(), accepted.job_id)
        .unwrap()
        .unwrap();
    assert!(stored.admitted_turn_id.is_none());
    drop(dispatcher);
    drop(jobs);
    let reopened = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let dispatcher = reopened
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    let receipt = call(&dispatcher, "delegate_task", task("before-start")).await;
    assert_eq!(receipt["job_id"], accepted.job_id.to_string());
    assert_eq!(receipt["state"], "interrupted");
    assert_eq!(count.load(Ordering::SeqCst), 0);
    reopened.cancel_profile(profile.id);
    server.abort();
}

#[tokio::test]
async fn remote_individual_cancel_before_admission_starts_no_provider_request() {
    let (endpoint, count, _, server) = provider(false).await;
    let (_temp, app, profile) = fixture(&endpoint).await;
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let _dispatcher = jobs
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    let profile_id = profile.id;
    let accepted = jobs
        .submit(move |service| async move {
            let scope = service
                .inner
                .state
                .lock()
                .unwrap()
                .profiles
                .get(&profile_id)
                .unwrap()
                .clone();
            let accepted = service
                .start_job(
                    &scope,
                    serde_json::from_value(task("individual-pre-stop")).unwrap(),
                    None,
                    CancellationToken::new(),
                )
                .await?;
            service
                .cancel_job_inner(profile_id, accepted.job_id)
                .await?;
            Ok(accepted)
        })
        .await
        .unwrap();
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(2)).await);
    let stored = app
        .store
        .remote_job_store()
        .get(&profile.id.0.to_string(), accepted.job_id)
        .unwrap()
        .unwrap();
    assert!(stored.admitted_turn_id.is_none());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    jobs.cancel_profile(profile.id);
    server.abort();
}

#[tokio::test]
async fn remote_job_survives_mcp_session_termination_and_returns_canonical_tool_result() {
    use crate::mcp_publish::transport::PublishHttpServer;
    use sha2::{Digest, Sha256};
    let (endpoint, count, released, server) = provider(true).await;
    let (_temp, app, profile) = fixture(&endpoint).await;
    let jobs = RemoteJobService::new(app.process_runtime.clone()).unwrap();
    let dispatcher = jobs
        .dispatcher(profile.clone(), app.config.clone(), vec![])
        .await
        .unwrap();
    const TOKEN: &str = "remote-agent-fixture-token-for-http-lifetime-01";
    let mut transport = PublishHttpServer::start(
        "127.0.0.1:0".parse().unwrap(),
        Sha256::digest(TOKEN.as_bytes()).into(),
        dispatcher.clone(),
        1,
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client.post(transport.endpoint()).bearer_auth(TOKEN).header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"remote-job-test","version":"1"}}})).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let request = |value: Value| {
        client
            .post(transport.endpoint())
            .bearer_auth(TOKEN)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", "2025-11-25")
            .json(&value)
    };
    assert_eq!(
        request(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let response: Value = request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"delegate_task","arguments":task("http-lifetime")}})).send().await.unwrap().json().await.unwrap();
    let id = response["result"]["structuredContent"]["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::time::timeout(Duration::from_secs(15), async {
        while count.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let response = client
        .delete(transport.endpoint())
        .bearer_auth(TOKEN)
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-11-25")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let running = call(&dispatcher, "task_status", json!({"job_id":id})).await;
    assert_eq!(running["state"], "running");
    released.notify_waiters();
    let finished = terminal(&dispatcher, &id).await;
    assert_eq!(finished["state"], "completed", "{finished}");
    assert_eq!(finished["result"], "WinB receiver result");
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let job = app
        .store
        .remote_job_store()
        .get(&profile.id.0.to_string(), id.parse().unwrap())
        .unwrap()
        .unwrap();
    let terminal = app
        .store
        .session_repo()
        .durable_terminal_for_turn(job.session_id, job.admitted_turn_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.tool_call_count, 1);
    jobs.cancel_profile(profile.id);
    assert!(jobs.drain_profile(profile.id, Duration::from_secs(2)).await);
    assert!(transport.stop().await);
    server.abort();
}
