use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn invalid_project_context_is_rejected_before_runner_admission_or_preparation() {
    for invalid in ["other-project", "oversized-overview", "invalid-origin"] {
        let (_temp, settings, path) = super::tests::fixture();
        let root = path.parent().unwrap();
        let paths = crate::storage::StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/db.sqlite3"),
            truncation_dir: root.join("data/output"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let process = crate::app::AppBootstrap::create_process_runtime(
            crate::storage::StoreBundle::new(sqlite),
        )
        .await
        .unwrap();
        let host = RunnerHost::from_process(process).unwrap();
        let mut assignment = super::tests::assignment();
        // A delivery after restart must be validated before even recording a
        // recovery intent. An input reference must not trigger materialization.
        assignment.job.state = JobState::Running;
        assignment.job.input = json!({"version":1,"prompt":"Task","input_refs":["asset"]});
        let mut context = crate::context::world_state::SharedProjectContext {
            project_id: assignment.job.project_id.clone(),
            label: "Project".into(),
            overview: "Worker and DB".into(),
            revision: "1".into(),
            root_prompt: "Complete the task".into(),
            origin_device_id: Some("WinA".into()),
        };
        match invalid {
            "other-project" => context.project_id = "another-project".into(),
            "oversized-overview" => context.overview = "x".repeat(8 * 1024 + 1),
            "invalid-origin" => context.origin_device_id = Some("invalid/id".into()),
            _ => unreachable!(),
        }
        assignment.project_context = Some(context);
        let (client, requests, server) =
            tls_script(root, vec![Some((200, json!([assignment])))]).await;
        let mut controller = Controller {
            host: host.clone(),
            settings: settings.clone(),
            client,
            journal: Journal::open(&path, &settings).unwrap(),
            checkpoint_cursor: String::new(),
            commands: None,
            external: None,
            local_project_id: None,
        };
        let (_, result) = controller.tick().await;
        server.await.unwrap();
        assert!(
            controller.journal.get("attempt").unwrap().is_none(),
            "{invalid}: invalid context must not create local admission evidence"
        );
        assert!(
            result.is_err(),
            "{invalid}: invalid context must be rejected"
        );
        let observed = requests.lock().unwrap();
        assert_eq!(observed.len(), 1, "no Started report or preparation read");
        assert!(observed[0].0.starts_with("/v1/shared/runner/assignments?"));
        assert!(!root.join(".moyai-shared-inputs-job").exists());
        assert!(
            host.inner.state.lock().unwrap().runs.is_empty(),
            "no LLM worker"
        );
        drop(observed);
        host.begin_shutdown().unwrap();
        host.wait_stopped().await;
    }
}

#[tokio::test]
async fn active_shared_execution_publishes_canonical_operations_before_finishing() {
    active_progress_delivery(true, false).await;
}

#[tokio::test]
async fn shared_progress_lost_ack_retries_without_finishing_the_worker() {
    active_progress_delivery(true, true).await;
}

#[tokio::test]
async fn shared_progress_is_not_sent_to_a_legacy_hub() {
    active_progress_delivery(false, false).await;
}

async fn active_progress_delivery(supported: bool, lose_first_ack: bool) {
    use crate::protocol::{HistoryItem, HistoryItemId, HistoryItemPayload, HistoryScope};
    use crate::runner::Execution;
    use crate::runtime::{OwnedTaskHandle, RunControl};
    use tokio_util::sync::CancellationToken;

    let (_temp, settings, journal_path) = super::tests::fixture();
    let root = journal_path.parent().unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let store = crate::storage::StoreBundle::new(sqlite);
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    store
        .protocol_event_store()
        .seed_history_item_for_test(&HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Session,
            sequence_no: 0,
            created_at_ms: 0,
            payload: HistoryItemPayload::CollaborationModeInstruction {
                mode: crate::protocol::ModeKind::Default,
            },
        })
        .unwrap();
    // Bootstrap reconciles abandoned admissions; admit this live fixture only
    // after process recovery, just as the real execution owner does.
    let process = crate::app::AppBootstrap::create_process_runtime(store.clone())
        .await
        .unwrap();
    assert!(
        store
            .session_repo()
            .admit_session_turn(session_id, turn_id)
            .await
            .unwrap()
            .is_some()
    );
    store
        .protocol_event_store()
        .seed_history_item_for_test(&HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 0,
            created_at_ms: 1,
            payload: HistoryItemPayload::Error {
                message: "公開する確定済み操作の診断".into(),
            },
        })
        .unwrap();

    let host = RunnerHost::from_process(process).unwrap();
    let mut value = serde_json::to_value(super::tests::assignment()).unwrap();
    value["supports_progress_reports"] = json!(supported);
    value["job"]["state"] = json!("running");
    let assignment: Assignment = serde_json::from_value(value.clone()).unwrap();
    let mut responses = vec![Some((200, json!([value.clone()])))];
    if supported {
        responses.push((!lose_first_ack).then(|| (200, json!(assignment.job))));
    }
    responses.push(Some((200, json!([value]))));
    if supported && lose_first_ack {
        responses.push(Some((200, json!(assignment.job))));
    }
    let (client, requests, server) = tls_script(root, responses).await;
    let mut journal = Journal::open(&journal_path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment, settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let control = RunControl::new();
    let token = control.token();
    let worker = OwnedTaskHandle::new(1, tokio::spawn(async move { token.cancelled().await }));
    host.inner.state.lock().unwrap().runs.insert(
        entry.run_id,
        Execution {
            managed_scope_id: entry.run_id,
            request: LocalRunRequest {
                directory: root.into(),
                prompt: "Work".into(),
                session_id: None,
                title: None,
                single_agent: true,
            },
            session_id: Some(session_id),
            control: control.clone(),
            process_lifetime: CancellationToken::new(),
            processes_drained: false,
            service: None,
            worker: Some(worker),
            result: None,
            response: None,
            shared: true,
            resource: None,
        },
    );
    let mut controller = Controller {
        host: host.clone(),
        settings,
        client,
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    controller.tick().await.1.unwrap();
    assert_eq!(
        controller
            .journal
            .get(&entry.assignment.attempt_id)
            .unwrap()
            .unwrap()
            .progress_revision
            > 0,
        supported && !lose_first_ack,
        "only a delivery ACK advances the journal cursor"
    );
    if lose_first_ack {
        // An unacknowledged public record must survive later private canonical
        // entries moving it out of the bounded tail used to discover new work.
        for index in 0..65 {
            host.inner
                .process
                .store()
                .protocol_event_store()
                .seed_history_item_for_test(&HistoryItem {
                    id: HistoryItemId::new(),
                    session_id,
                    scope: HistoryScope::Turn { turn_id },
                    sequence_no: 0,
                    created_at_ms: index + 2,
                    payload: HistoryItemPayload::UserTurn {
                        content: vec![],
                        prompt_dispatch: None,
                        editor_context: None,
                    },
                })
                .unwrap();
        }
    }
    controller.tick().await.1.unwrap();
    assert!(!control.is_cancelled());
    assert_eq!(
        controller
            .journal
            .get(&entry.assignment.attempt_id)
            .unwrap()
            .unwrap()
            .phase,
        Phase::Executing
    );
    let observed = requests.lock().unwrap().clone();
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
    server.abort();
    let _ = server.await;
    let reports = observed
        .iter()
        .filter(|(_, body)| body["outcome"]["kind"] == "progress")
        .collect::<Vec<_>>();
    assert_eq!(
        reports.len(),
        if !supported {
            0
        } else if lose_first_ack {
            2
        } else {
            1
        },
        "public progress is delivered before terminal and unchanged canonical history is not republished"
    );
    if supported {
        assert_eq!(
            reports[0].1["outcome"]["items"][0]["payload"]["message"],
            "公開する確定済み操作の診断"
        );
        if lose_first_ack {
            assert_eq!(reports[0].1, reports[1].1);
        }
    }
    let acknowledged = controller
        .journal
        .get(&entry.assignment.attempt_id)
        .unwrap()
        .unwrap()
        .progress_revision;
    let settings = controller.settings.clone();
    drop(controller);
    let reopened = Journal::open(&journal_path, &settings).unwrap();
    assert_eq!(
        reopened
            .get(&entry.assignment.attempt_id)
            .unwrap()
            .unwrap()
            .progress_revision,
        acknowledged
    );
}

async fn tls_script(
    root: &camino::Utf8Path,
    responses: Vec<Option<(u16, serde_json::Value)>>,
) -> (
    SharedClient,
    Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    tokio::task::JoinHandle<()>,
) {
    tls_script_with_delays(
        root,
        responses
            .into_iter()
            .map(|response| (Duration::ZERO, response))
            .collect(),
    )
    .await
}

async fn tls_script_with_delays(
    root: &camino::Utf8Path,
    responses: Vec<(Duration, Option<(u16, serde_json::Value)>)>,
) -> (
    SharedClient,
    Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    tokio::task::JoinHandle<()>,
) {
    tls_script_connections(root, responses, false).await
}

async fn tls_script_connections(
    root: &camino::Utf8Path,
    responses: Vec<(Duration, Option<(u16, serde_json::Value)>)>,
    keep_alive: bool,
) -> (
    SharedClient,
    Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    tokio::task::JoinHandle<()>,
) {
    use crate::device_network::{DeviceClient, DeviceIdentityStore, SharedHubConfig};
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    let ca = ca_params.self_signed(&ca_key).unwrap().pem();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let server_key = rcgen::KeyPair::generate().unwrap();
    let mut server_params = rcgen::CertificateParams::new(vec![
        "127.0.0.1".into(),
        crate::mcp_publish::tls::HUB_TLS_ROLE_NAME.into(),
    ])
    .unwrap();
    server_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let server_certificate = server_params.signed_by(&server_key, &issuer).unwrap().pem();
    let acceptor = crate::mcp_publish::tls::load_mtls_acceptor(
        &server_certificate,
        &server_key.serialize_pem(),
        &ca,
    )
    .unwrap();
    let identity = DeviceIdentityStore::new(root.join("identity.json"))
        .load_or_create()
        .unwrap();
    let key = rcgen::KeyPair::from_pem(identity.private_key_pem()).unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let certificate = params.signed_by(&key, &issuer).unwrap().pem();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = SharedHubConfig {
        hub_url: format!("https://{}", listener.local_addr().unwrap()),
        ca_certificate_pem: ca,
    };
    let client = SharedClient::for_test(
        DeviceClient::new(&config, Some((&identity, &certificate)), "device".into()).unwrap(),
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = tokio::spawn(async move {
        let mut connection = None;
        for (delay, response) in responses {
            let mut stream = match connection.take() {
                Some(stream) => stream,
                None => {
                    let (tcp, _) = listener.accept().await.unwrap();
                    acceptor.accept(tcp).await.unwrap()
                }
            };
            let mut header = Vec::new();
            // A client may close a keep-alive connection instead of reusing it.
            match stream.read_u8().await {
                Ok(byte) => header.push(byte),
                Err(error) if keep_alive && error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    let (tcp, _) = listener.accept().await.unwrap();
                    stream = acceptor.accept(tcp).await.unwrap();
                }
                Err(error) => panic!("HTTP request read failed: {error}"),
            }
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
                assert!(header.len() < 16384);
            }
            let header = String::from_utf8(header).unwrap();
            let path = header.split_whitespace().nth(1).unwrap().to_string();
            if path == "/v1/shared/runner/claim"
                || path.starts_with("/v1/shared/runner/assignments?")
                || (path.starts_with("/v1/shared/runner/attempts/")
                    && !path.contains("/authorize")
                    && !path.contains("/assets/")
                    && !path.ends_with("/archive"))
            {
                let capabilities = header
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("x-moyai-runner-capabilities"))
                    .map(|(_, value)| value.trim());
                assert_eq!(
                    capabilities,
                    Some(super::protocol::MULTI_DEVICE_SESSION_CAPABILITY),
                    "Runner must advertise its exact protocol before assignment delivery"
                );
            }
            let length = header
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            assert!(length < 256 * 1024);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            captured.lock().unwrap().push((
                path,
                serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
            ));
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if let Some((status, body)) = response {
                let body = serde_json::to_string(&body).unwrap();
                let mode = if keep_alive { "keep-alive" } else { "close" };
                stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {mode}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            if keep_alive {
                connection = Some(stream);
            } else {
                stream.shutdown().await.unwrap();
            }
        }
    });
    (client, requests, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effect_authorization_progresses_after_http_used_on_the_local_agent_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(temp.path()).unwrap();
    let (client, requests, server) = tls_script_connections(
        root,
        vec![
            (Duration::ZERO, Some((200, json!({"ready":true})))),
            (Duration::ZERO, Some((200, json!({"authorized":true})))),
        ],
        true,
    )
    .await;
    let authority = client.effect_authority(super::tests::assignment());
    let executor = crate::runtime::LocalTaskExecutor::new("effect-authority-pool-test").unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = executor
        .spawn(1, move || async move {
            // Model preparation/approval runs here. Its idle HTTP connection must not
            // prevent the host runtime from authorizing a synchronous final-effect fence.
            let _: serde_json::Value = client.request("/v1/ready", None).await.unwrap();
            tokio::task::yield_now().await;
            let result = authority.authorize(Some("approved-effect"));
            let _ = send.send(result);
        })
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(13), receive)
        .await
        .unwrap()
        .unwrap();
    worker.detach();
    server.abort();
    let _ = server.await;
    assert!(
        result.is_ok(),
        "effect authorization must progress: {result:?}"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "No request may be replayed");
    assert!(requests[1].0.ends_with("/authorize"));
    assert_eq!(requests[1].1["approval_id"], "approved-effect");
}

#[tokio::test]
async fn project_folder_binding_uses_existing_contents_and_reports_current_participation() {
    for template_id in ["local-folder", "desktop-default"] {
        let (_temp, mut settings, path) = super::tests::fixture();
        settings.environments.clear();
        let root = path.parent().unwrap();
        let selected = root.join("human-existing");
        std::fs::create_dir(&selected).unwrap();
        std::fs::write(selected.join("ProjectBrief.md"), "partial work").unwrap();
        let (client, requests, server) = tls_script(
        root,
        vec![
            Some((200, json!([{"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":false}]))),
            Some((200, json!([{"environment_id":"fresh-env","template_id":template_id,"generation":1}]))),
            Some((200, json!({"saved":true}))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":false}
            ]))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":false}
            ]))),
            Some((200, json!([{"environment_id":"next-env","template_id":template_id,"generation":1}]))),
            Some((200, json!({"saved":true}))),
            Some((200, json!({"saved":true}))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":true}
            ]))),
            Some((200, json!({"saved":true}))),
            Some((200, json!([{"environment_id":"next-env","template_id":template_id,"generation":2,"state":"failed"}]))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":false}
            ]))),
            Some((200, json!([{"environment_id":"next-env","template_id":template_id,"generation":2,"state":"failed"}]))),
            Some((200, json!({"saved":true}))),
        ],
    )
    .await;
        let paths = crate::storage::StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/db.sqlite3"),
            truncation_dir: root.join("data/output"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let process = crate::app::AppBootstrap::create_process_runtime(
            crate::storage::StoreBundle::new(sqlite),
        )
        .await
        .unwrap();
        let host = RunnerHost::from_process(process).unwrap();
        let journal = Journal::open(&path, &settings).unwrap();
        let mut controller = Controller {
            host: host.clone(),
            settings,
            client,
            journal,
            checkpoint_cursor: String::new(),
            commands: None,
            external: None,
            local_project_id: None,
        };
        controller
            .bind_project_folder(
                "project",
                "fresh-env",
                selected.clone(),
                crate::config::AccessMode::Default,
                None,
            )
            .await
            .unwrap();
        let mapping = controller.settings.mapping("fresh-env").unwrap().clone();
        assert_eq!(
            mapping.directory,
            camino::Utf8PathBuf::from_path_buf(std::fs::canonicalize(&selected).unwrap()).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(selected.join("ProjectBrief.md")).unwrap(),
            "partial work"
        );
        assert_eq!(std::fs::read_dir(&selected).unwrap().count(), 1);
        let saved = host.installed_shared_settings().unwrap().unwrap();
        assert_eq!(saved.mapping("fresh-env").unwrap(), &mapping);
        controller.validated_resource_catalog().await.unwrap();
        assert!(controller.settings.mapping("fresh-env").is_err());
        assert!(controller.settings.mapping("next-env").is_err());
        controller
            .bind_project_folder(
                "project",
                "next-env",
                selected.clone(),
                crate::config::AccessMode::Default,
                None,
            )
            .await
            .unwrap();
        assert!(controller.settings.mapping("fresh-env").is_err());
        assert_eq!(
            controller.settings.mapping("next-env").unwrap().directory,
            mapping.directory
        );
        let moved = root.join("human-moved");
        std::fs::rename(&selected, &moved).unwrap();
        controller.sync_provisioning().await.unwrap();
        assert!(controller.settings.mapping("next-env").is_err());
        assert_eq!(
            std::fs::read_to_string(moved.join("ProjectBrief.md")).unwrap(),
            "partial work"
        );
        controller
            .bind_project_folder(
                "project",
                "next-env",
                moved.clone(),
                crate::config::AccessMode::Default,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            controller.settings.mapping("next-env").unwrap().directory,
            camino::Utf8PathBuf::from_path_buf(std::fs::canonicalize(&moved).unwrap()).unwrap()
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        let observed = requests.lock().unwrap();
        assert_eq!(
            observed
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/v1/shared/runner/environments?current_only=true",
                "/v1/shared/runner/provisioning",
                "/v1/shared/runner/provisioning",
                "/v1/shared/runner/environments?current_only=true",
                "/v1/shared/runner/environments?current_only=true",
                "/v1/shared/runner/provisioning",
                "/v1/shared/runner/provisioning",
                "/v1/shared/runner/templates",
                "/v1/shared/runner/environments?current_only=true",
                "/v1/shared/runner/provisioning",
                "/v1/shared/runner/provisioning",
                "/v1/shared/runner/environments?current_only=true",
                "/v1/shared/runner/provisioning",
                "/v1/shared/runner/provisioning",
            ]
        );
        assert_eq!(
            observed[2].1,
            json!({"environment_id":"fresh-env","template_id":template_id,"generation":1,"success":true,"error":null})
        );
        assert_eq!(
            observed[6].1,
            json!({"environment_id":"next-env","template_id":template_id,"generation":1,"success":true,"error":null})
        );
        assert_eq!(observed[9].1["success"], false);
        assert_eq!(observed[9].1["template_id"], template_id);
        assert_eq!(
            observed[13].1,
            json!({"environment_id":"next-env","template_id":template_id,"generation":2,"success":true,"error":null})
        );
        drop(observed);
        drop(controller);
        host.begin_shutdown().unwrap();
        host.wait_stopped().await;
    }
}

#[tokio::test]
async fn complete_environment_catalog_retires_only_drained_project_folder_permissions() {
    for automatic in [true, false] {
        let (_temp, settings, path) = super::tests::fixture();
        let root = path.parent().unwrap();
        let directory = settings.environments[0].directory.clone();
        let sentinel = directory.join("human-work.txt");
        std::fs::write(&sentinel, "keep me").unwrap();
        let row = json!({"id":"environment","resource_id":"device-resource","capacity":1,
            "project_ids":[],"enabled":false,"workspace_bound":true});
        let (client, _requests, server) = tls_script(
            root,
            vec![
                Some((200, json!([row.clone()]))), // Archive retains the current binding.
                Some((503, json!({"error":"unavailable"}))),
                Some((200, json!({"items":[],"next_before":"more"}))), // Not a complete array.
                Some((200, json!(vec![row; 129]))),
                Some((200, json!([]))), // Missing while a journal entry still needs settlement.
                Some((200, json!([]))), // Complete catalog after settlement.
            ],
        )
        .await;
        let paths = crate::storage::StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/db.sqlite3"),
            truncation_dir: root.join("data/output"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let process = crate::app::AppBootstrap::create_process_runtime(
            crate::storage::StoreBundle::new(sqlite),
        )
        .await
        .unwrap();
        let host = RunnerHost::from_process(process).unwrap();
        {
            let mut store = host.inner.operations.lock().unwrap();
            let mut installed = store.installed.clone();
            installed.settings = Some(settings.clone());
            if automatic {
                installed.provisions.push(serde_json::from_value(json!({
                    "environment_id":"environment","template_id":"local-folder","generation":1,"success":true,"error":null
                })).unwrap());
            }
            store.update(installed).unwrap();
        }
        let journal = Journal::open(&path, &settings).unwrap();
        let mut controller = Controller {
            host: host.clone(),
            settings,
            client,
            journal,
            checkpoint_cursor: String::new(),
            commands: None,
            external: None,
            local_project_id: None,
        };
        controller.validated_resource_catalog().await.unwrap();
        for _ in 0..3 {
            assert!(controller.validated_resource_catalog().await.is_err());
            assert!(controller.settings.mapping("environment").is_ok());
        }
        let mut entry = controller
            .journal
            .intent(
                super::tests::assignment(),
                controller.settings.environments[0].clone(),
            )
            .unwrap();
        controller.journal.executing(&mut entry).unwrap();
        controller
            .journal
            .uncertain(&mut entry, "Stop must be confirmed")
            .unwrap();
        assert!(controller.validated_resource_catalog().await.is_err());
        assert!(
            controller.settings.mapping("environment").is_ok(),
            "stop/settlement must precede revocation of the local binding"
        );
        let report = Report::for_assignment(
            &entry.assignment,
            "settled",
            ReportOutcome::Finished {
                success: false,
                result: json!({"stopped":true}),
                resources_released: true,
            },
        );
        controller.journal.reconciled(&mut entry, report).unwrap();
        controller.journal.settled(&mut entry).unwrap();
        let result = controller.validated_resource_catalog().await;
        assert_eq!(result.is_ok(), automatic);
        assert_eq!(
            controller.settings.mapping("environment").is_err(),
            automatic,
            "only a recorded project-folder permission may be retired automatically"
        );
        let saved = host.installed_shared_settings().unwrap().unwrap();
        assert_eq!(saved.mapping("environment").is_err(), automatic);
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "keep me");
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn leaving_idle_project_preserves_another_projects_retained_service() {
    retained_service_survives_local_catalog_change(false).await;
}

#[tokio::test]
async fn retained_report_failure_cannot_delay_another_services_observed_stop_fence() {
    use crate::tool::shell::{RetainedService, RetainedServiceState};
    use tokio_util::sync::CancellationToken;

    let (_temp, settings, path) = super::tests::fixture();
    let root = path.parent().unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut unknown = journal
        .intent(super::tests::assignment(), settings.environments[0].clone())
        .unwrap();
    let mut second_assignment = super::tests::assignment();
    second_assignment.attempt_id = "attempt-z".into();
    let mut live = journal
        .intent(second_assignment, settings.environments[0].clone())
        .unwrap();
    let shells = host
        .inner
        .process
        .managed_shells()
        .with_lifetime(CancellationToken::new(), live.run_id);
    let live_service = shells
        .start_preview_for_test(crate::session::SessionId::new())
        .await;
    let unknown_service = RetainedService {
        service_id: ulid::Ulid::new(),
        expires_at_ms: None,
        retain_after_turn: true,
    };
    for (entry, service) in [(&mut unknown, unknown_service), (&mut live, live_service)] {
        journal.executing(entry).unwrap();
        let report = Report::for_assignment(
            &entry.assignment,
            "outcome",
            ReportOutcome::Finished {
                success: true,
                result: json!({"text":"Preview"}),
                resources_released: true,
            },
        );
        journal
            .outcome_with_retention(entry, report, None, Some(service))
            .unwrap();
        journal.retention_reported(entry).unwrap();
        journal.settled(entry).unwrap();
    }
    let leases = [(&unknown, unknown_service, false), (&live, live_service, true)].map(|(entry, service, stop)| json!({
        "service_id":service.service_id.to_string(),"attempt_id":entry.assignment.attempt_id,
        "generation":entry.assignment.generation,"conversation_id":entry.assignment.job.conversation_id,
        "environment_id":entry.assignment.job.environment_id,"expires_at_ms":null,"stop_requested":stop,
    }));
    let (client, requests, server) = tls_script(
        root,
        vec![
            Some((200, json!(leases))),
            Some((503, json!({"error":"report temporarily unavailable"}))),
        ],
    )
    .await;
    let mut controller = Controller {
        host: host.clone(),
        settings,
        client,
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    assert!(controller.reconcile_retained_services().await.is_err());
    let service_state = shells.retained_service_state(live_service);
    let unknown_after = controller
        .journal
        .get(&unknown.assignment.attempt_id)
        .unwrap()
        .unwrap();
    let live_after = controller
        .journal
        .get(&live.assignment.attempt_id)
        .unwrap()
        .unwrap();
    server.await.unwrap();
    let reports = requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, body)| body["outcome"]["kind"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    shells.cancel_retained_service(live_service.service_id);
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
    assert_ne!(
        service_state,
        RetainedServiceState::Running,
        "an earlier report failure must not delay a stop already observed in the same lease response"
    );
    assert_eq!(reports, ["service_uncertain"]);
    assert!(!unknown_after.service_uncertain_ack);
    assert!(!live_after.service_stopped_ack);
    assert_eq!(
        controller.journal.retained_services().unwrap().len(),
        2,
        "neither an attempted report nor local stop releases unacknowledged capacity"
    );
}

#[tokio::test]
async fn local_catalog_error_does_not_reclassify_live_hub_control_or_stop_retained_service() {
    retained_service_survives_local_catalog_change(true).await;
}

async fn retained_service_survives_local_catalog_change(invalid_capacity: bool) {
    use crate::runner::Execution;
    use crate::runtime::{OwnedTaskHandle, RunControl};
    use crate::tool::shell::RetainedServiceState;
    use tokio_util::sync::CancellationToken;

    let (_temp, mut settings, path) = super::tests::fixture();
    let root = path.parent().unwrap();
    settings.resource_scope = ResourceScope::Device;
    settings.environments[0].directory = root.join("active-project");
    std::fs::create_dir(&settings.environments[0].directory).unwrap();
    let mut retired = settings.environments[0].clone();
    retired.environment_id = "retired-environment".into();
    retired.directory = root.join("retired-project");
    std::fs::create_dir(&retired.directory).unwrap();
    std::fs::write(retired.directory.join("human-work.txt"), "keep me").unwrap();
    settings.environments.push(retired.clone());
    settings.resolve().unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    {
        let mut store = host.inner.operations.lock().unwrap();
        let mut installed = store.installed.clone();
        installed.settings = Some(settings.clone());
        installed.provisions.push(
            serde_json::from_value(json!({
                "environment_id":retired.environment_id,"template_id":"local-folder",
                "generation":1,"success":true,"error":null
            }))
            .unwrap(),
        );
        store.update(installed).unwrap();
    }
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(super::tests::assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let session = crate::session::SessionId::new();
    let lifetime = CancellationToken::new();
    let shells = host
        .inner
        .process
        .managed_shells()
        .with_lifetime(lifetime.clone(), entry.run_id);
    let service = shells.start_preview_for_test(session).await;
    let worker = OwnedTaskHandle::new(1, tokio::spawn(async {}));
    while !worker.is_finished() {
        tokio::task::yield_now().await;
    }
    host.inner.state.lock().unwrap().runs.insert(
        entry.run_id,
        Execution {
            managed_scope_id: entry.run_id,
            request: LocalRunRequest {
                directory: settings.environments[0].directory.clone(),
                prompt: "Retain preview".into(),
                session_id: Some(session),
                title: None,
                single_agent: true,
            },
            session_id: Some(session),
            control: RunControl::new(),
            process_lifetime: lifetime,
            processes_drained: false,
            service: None,
            worker: Some(worker),
            result: None,
            response: None,
            shared: true,
            resource: None,
        },
    );
    let report = Report::for_assignment(
        &entry.assignment,
        "outcome",
        ReportOutcome::Finished {
            success: true,
            result: json!({"text":"Preview is running"}),
            resources_released: true,
        },
    );
    journal
        .outcome_with_retention(&mut entry, report, None, Some(service))
        .unwrap();
    journal.retention_reported(&mut entry).unwrap();
    journal.settled(&mut entry).unwrap();
    let catalog = json!([{"id":"environment","resource_id":"device-resource",
        "capacity":if invalid_capacity {2} else {1}, "project_ids":["project"],
        "enabled":true,"workspace_bound":true}]);
    let mut responses = vec![
        Some((200, json!([]))),
        Some((
            200,
            json!([{"service_id":service.service_id.to_string(),
            "attempt_id":entry.assignment.attempt_id,"generation":entry.assignment.generation,
            "conversation_id":entry.assignment.job.conversation_id,
            "environment_id":"environment","expires_at_ms":null,"stop_requested":false}]),
        )),
        Some((200, json!({"saved":true}))),
        Some((200, catalog.clone())),
    ];
    if !invalid_capacity {
        responses.extend([
            Some((200, json!([]))),
            Some((200, catalog)),
            Some((200, json!(null))),
        ]);
    }
    let (client, requests, server) = tls_script(root, responses).await;
    let controller = Controller {
        host: host.clone(),
        settings,
        client,
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    let task = tokio::spawn(controller.run());
    let projection = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(projection) = host.inner.state.lock().unwrap().shared_projection.clone() {
                break projection;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Capture the first completed control cycle before the fixture ends its transport.
    let service_state = shells.retained_service_state(service);
    let installed = host.installed_shared_settings().unwrap().unwrap();
    let claims = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == "/v1/shared/runner/claim")
        .count();
    task.abort();
    let _ = task.await;
    server.abort();
    let _ = server.await;
    shells.cancel_retained_service(service.service_id);
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
    assert_eq!(
        service_state,
        RetainedServiceState::Running,
        "unrelated local catalog changes must not stop an acknowledged service"
    );
    assert!(
        projection.connected,
        "the assignment and service control polls succeeded"
    );
    assert_eq!(projection.error.is_some(), invalid_capacity);
    assert!(!projection.retained_services.is_empty());
    assert_eq!(claims, usize::from(!invalid_capacity));
    assert_eq!(
        installed.mapping(&retired.environment_id).is_ok(),
        invalid_capacity
    );
    assert!(installed.mapping("environment").is_ok());
    assert_eq!(
        std::fs::read_to_string(retired.directory.join("human-work.txt")).unwrap(),
        "keep me"
    );
}

#[tokio::test]
async fn legacy_missing_folder_rebind_keeps_authority_guards_and_recovers_a_lost_ack() {
    let (_temp, mut settings, path) = super::tests::fixture();
    let root = path.parent().unwrap();
    let selected = root.join("original-work");
    let moved = root.join("moved-work");
    std::fs::create_dir(&selected).unwrap();
    std::fs::write(selected.join("ProjectBrief.md"), "human work").unwrap();
    settings.environments[0].directory = selected.clone();
    settings.resolve().unwrap();
    let old_mapping = settings.environments[0].clone();
    let catalog = |project: &str, enabled: bool| {
        json!([{
            "id":"environment", "resource_id":"device-resource", "capacity":1,
            "project_ids":[project], "enabled":enabled
        }])
    };
    let pending = json!([{"environment_id":"environment", "template_id":"desktop-default", "generation":2, "state":"failed"}]);
    let mut responses = vec![
        Some((200, json!({"saved":true}))),
        Some((200, catalog("project", true))),
        Some((200, json!({"saved":true}))),
        Some((200, pending.clone())),
        Some((200, catalog("another-project", false))),
    ];
    // Stale selection, active work, uncertain work, retained app, then an explicit rebind.
    for _ in 0..5 {
        responses.push(Some((200, catalog("project", false))));
        responses.push(Some((200, pending.clone())));
    }
    responses.push(None); // The local rebind is durable, but the Hub acknowledgement is lost.
    responses.extend([
        Some((200, json!({"saved":true}))),
        Some((200, catalog("project", false))),
        Some((200, pending.clone())),
        Some((200, json!({"saved":true}))),
    ]);
    let (client, requests, server) = tls_script(root, responses).await;
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    {
        let mut store = host.inner.operations.lock().unwrap();
        let mut installed = store.installed.clone();
        installed.settings = Some(settings.clone());
        installed.provisions.push(
            serde_json::from_value(json!({
                "environment_id":"environment", "template_id":"desktop-default",
                "generation":1, "success":true, "error":null
            }))
            .unwrap(),
        );
        store.update(installed).unwrap();
    }
    std::fs::rename(&selected, &moved).unwrap();
    {
        let mut store = host.inner.operations.lock().unwrap();
        settings
            .resolve_installed(&store.installed.provisions)
            .unwrap();
        let mut installed = store.installed.clone();
        installed.settings = Some(settings.clone());
        store.update(installed).unwrap();
    }
    let journal = Journal::open(&path, &settings).unwrap();
    let mut controller = Controller {
        host: host.clone(),
        settings,
        client,
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    controller.sync_provisioning().await.unwrap();
    assert_eq!(
        requests.lock().unwrap().len(),
        4,
        "Missing legacy folders must wait for explicit selection"
    );
    assert!(!selected.exists());
    assert!(controller.settings.mapping("environment").is_err());
    let access = crate::config::AccessMode::Default;
    assert!(
        controller
            .bind_project_folder("project", "environment", moved.clone(), access, None)
            .await
            .unwrap_err()
            .message
            .contains("no longer belongs")
    );
    assert!(
        controller
            .bind_project_folder(
                "project",
                "environment",
                moved.clone(),
                access,
                Some(old_mapping.directory.clone())
            )
            .await
            .unwrap_err()
            .message
            .contains("previously selected")
    );
    let mut entry = controller
        .journal
        .intent(super::tests::assignment(), old_mapping.clone())
        .unwrap();
    assert!(
        controller
            .bind_project_folder("project", "environment", moved.clone(), access, None)
            .await
            .unwrap_err()
            .message
            .contains("Stop this PC")
    );
    controller.journal.executing(&mut entry).unwrap();
    controller
        .journal
        .uncertain(&mut entry, "Restart requires review")
        .unwrap();
    assert!(
        controller
            .bind_project_folder("project", "environment", moved.clone(), access, None)
            .await
            .unwrap_err()
            .message
            .contains("Stop this PC")
    );
    let report = Report::for_assignment(
        &entry.assignment,
        "reviewed",
        ReportOutcome::Finished {
            success: false,
            result: json!({"reviewed":true}),
            resources_released: true,
        },
    );
    controller.journal.reconciled(&mut entry, report).unwrap();
    controller.journal.settled(&mut entry).unwrap();
    let mut assignment = super::tests::assignment();
    assignment.attempt_id = "retained-attempt".into();
    let mut retained = controller.journal.intent(assignment, old_mapping).unwrap();
    let report = Report::for_assignment(
        &retained.assignment,
        "preview",
        ReportOutcome::Finished {
            success: true,
            result: json!({"preview":true}),
            resources_released: false,
        },
    );
    controller
        .journal
        .outcome_with_retention(
            &mut retained,
            report,
            None,
            Some(crate::tool::shell::RetainedService {
                service_id: ulid::Ulid::new(),
                expires_at_ms: None,
                retain_after_turn: true,
            }),
        )
        .unwrap();
    controller.journal.settled(&mut retained).unwrap();
    assert!(
        controller
            .bind_project_folder("project", "environment", moved.clone(), access, None)
            .await
            .unwrap_err()
            .message
            .contains("Stop this PC")
    );
    controller.journal.service_stopped(&mut retained).unwrap();
    assert!(
        controller
            .bind_project_folder("project", "environment", moved.clone(), access, None)
            .await
            .is_err()
    );
    let reopened = crate::runner::operations::OperationsStore::open(&paths.data_dir).unwrap();
    controller.settings = reopened.installed.settings.clone().unwrap();
    controller
        .settings
        .resolve_installed(&reopened.installed.provisions)
        .unwrap();
    assert_eq!(
        controller
            .settings
            .mapping("environment")
            .unwrap()
            .directory,
        camino::Utf8PathBuf::from_path_buf(std::fs::canonicalize(&moved).unwrap()).unwrap()
    );
    controller.sync_provisioning().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let observed = requests.lock().unwrap();
    assert_eq!(observed.len(), 20);
    let reports = observed
        .iter()
        .filter(|(path, body)| path == "/v1/shared/runner/provisioning" && body.is_object())
        .collect::<Vec<_>>();
    assert_eq!(reports.len(), 3);
    for (_, report) in &reports {
        assert_eq!(report["template_id"], "desktop-default");
    }
    assert_eq!(reports[0].1["generation"], 1);
    assert_eq!(reports[1].1["generation"], 2);
    assert_eq!(reports[2].1["generation"], 2);
    assert_eq!(reports[0].1["success"], false);
    assert_eq!(reports[1].1["success"], true);
    assert_eq!(reports[2].1["success"], true);
    assert!(!selected.exists());
    assert_eq!(
        std::fs::read_to_string(moved.join("ProjectBrief.md")).unwrap(),
        "human work"
    );
    drop(observed);
    drop(controller);
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
}

#[tokio::test]
async fn continuation_uses_only_the_exact_predecessor_archive() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};

    for source in ["cancelled", "predecessor"] {
        let (_temp, settings, path) = super::tests::fixture();
        let content = serde_json::to_vec(&json!({"version":1,"job_id":source,"project_id":"project","environment_id":"environment","transcript":[]})).unwrap();
        let download = json!({"asset":{
            "id":"archive", "project_id":"project", "job_id":source,
            "kind":"archive", "name":"canonical-history.json",
            "sha256":format!("{:x}",Sha256::digest(&content)),
            "byte_length":content.len()
        },"content_base64":STANDARD.encode(&content)});
        let (client, _, server) =
            tls_script(path.parent().unwrap(), vec![Some((200, download))]).await;
        let mut journal = Journal::open(&path, &settings).unwrap();
        let mut assignment = super::tests::assignment();
        assignment.job.continued_from = Some("predecessor".into());
        let entry = journal
            .intent(assignment, settings.environments[0].clone())
            .unwrap();
        let input = SharedInput {
            version: 1,
            prompt: "Continue".into(),
            input_refs: vec![],
        };
        let result = super::data::prepare(&client, &entry, &input).await;
        if source == "predecessor" {
            assert_eq!(
                result.unwrap().continuation.unwrap().previous_job_id,
                source
            );
        } else {
            assert!(result.err().unwrap().message.contains("exact predecessor"));
        }
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn preparation_retries_busy_reads_without_replaying_started_or_materialization() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};

    let (_temp, settings, path) = super::tests::fixture();
    let root = path.parent().unwrap();
    let mut assignment = super::tests::assignment();
    assignment.job.state = JobState::Running;
    assignment.job.input = json!({"version":2,"prompt":"Read the input","input_refs":["input"]});
    let input: SharedInput = serde_json::from_value(assignment.job.input.clone()).unwrap();
    let content = b"original input";
    let download = json!({"asset":{"id":"input","project_id":"project","job_id":null,
        "kind":"input","name":"input.txt","sha256":format!("{:x}",Sha256::digest(content)),
        "byte_length":content.len()},"content_base64":STANDARD.encode(content)});
    let (client, captured, server) = tls_script(
        root,
        vec![
            Some((200, serde_json::to_value(&assignment.job).unwrap())),
            Some((429, json!({"error":"capacity"}))),
            Some((200, download)),
            Some((429, json!({"error":"capacity"}))),
            Some((200, serde_json::Value::Null)),
        ],
    )
    .await;
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment, settings.environments[0].clone())
        .unwrap();
    client
        .report(&Report::for_assignment(
            &entry.assignment,
            "started",
            ReportOutcome::Started,
        ))
        .await
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let prepared = super::data::prepare(&client, &entry, &input).await;
    assert!(
        prepared.is_ok(),
        "temporary data-slot pressure must not fail an accepted job: {:?}",
        prepared.err()
    );
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(root.join(".moyai-shared-inputs-job/input.txt")).unwrap(),
        content
    );
    let observed = captured.lock().unwrap();
    assert_eq!(
        observed
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/v1/shared/runner/report",
            "/v1/shared/runner/attempts/attempt/assets/input",
            "/v1/shared/runner/attempts/attempt/assets/input",
            "/v1/shared/runner/attempts/attempt/archive",
            "/v1/shared/runner/attempts/attempt/archive",
        ]
    );
    assert_eq!(observed[0].1["outcome"]["kind"], "started");
    assert_eq!(entry.phase, Phase::Executing);
    assert!(entry.report.is_none());
}

#[tokio::test]
async fn preparation_busy_retry_preserves_later_permission_rejection() {
    for status in [401, 403, 404] {
        let (_temp, settings, path) = super::tests::fixture();
        let (client, captured, server) = tls_script(
            path.parent().unwrap(),
            vec![
                Some((429, json!({"error":"capacity"}))),
                Some((status, json!({"error":"denied"}))),
            ],
        )
        .await;
        let mut journal = Journal::open(&path, &settings).unwrap();
        let mut entry = journal
            .intent(super::tests::assignment(), settings.environments[0].clone())
            .unwrap();
        journal.executing(&mut entry).unwrap();
        let input: SharedInput =
            serde_json::from_value(entry.assignment.job.input.clone()).unwrap();
        let error = super::data::prepare(&client, &entry, &input)
            .await
            .err()
            .unwrap();
        assert!(
            error.message.contains(&format!("HTTP {status}")),
            "{}",
            error.message
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(captured.lock().unwrap().len(), 2);
        assert_eq!(entry.phase, Phase::Executing);
        assert!(entry.report.is_none());
    }
}

#[tokio::test]
async fn preparation_retry_does_not_extend_to_control_mutations_or_ambiguous_reads() {
    let (_temp, settings, path) = super::tests::fixture();
    let (client, captured, server) = tls_script(
        path.parent().unwrap(),
        vec![Some((429, json!({"error":"capacity"}))), None],
    )
    .await;
    assert!(matches!(
        client
            .report(&Report::for_assignment(
                &super::tests::assignment(),
                "started",
                ReportOutcome::Started
            ))
            .await,
        Err(TransportError::Rejected(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ))
    ));
    let mut journal = Journal::open(&path, &settings).unwrap();
    let entry = journal
        .intent(super::tests::assignment(), settings.environments[0].clone())
        .unwrap();
    let input: SharedInput = serde_json::from_value(entry.assignment.job.input.clone()).unwrap();
    let error = super::data::prepare(&client, &entry, &input)
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("unavailable"), "{}", error.message);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(captured.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn preparation_busy_reads_have_one_deadline_even_when_a_later_response_stalls() {
    for stall in [false, true] {
        let (_temp, settings, path) = super::tests::fixture();
        let responses = if stall {
            vec![
                (Duration::ZERO, Some((429, json!({"error":"capacity"})))),
                (
                    Duration::from_secs(7),
                    Some((429, json!({"error":"capacity"}))),
                ),
                (Duration::from_secs(8), Some((200, serde_json::Value::Null))),
            ]
        } else {
            vec![(Duration::ZERO, Some((429, json!({"error":"capacity"})))); 100]
        };
        let (client, captured, server) =
            tls_script_with_delays(path.parent().unwrap(), responses).await;
        let mut journal = Journal::open(&path, &settings).unwrap();
        let mut entry = journal
            .intent(super::tests::assignment(), settings.environments[0].clone())
            .unwrap();
        journal.executing(&mut entry).unwrap();
        let input: SharedInput =
            serde_json::from_value(entry.assignment.job.input.clone()).unwrap();
        let started = std::time::Instant::now();
        let error = super::data::prepare(&client, &entry, &input)
            .await
            .err()
            .unwrap();
        let elapsed = started.elapsed();
        server.abort();
        let _ = server.await;
        assert!(
            elapsed >= Duration::from_secs(9) && elapsed < Duration::from_secs(12),
            "one read must retain its ten-second deadline across all retries: {elapsed:?}, {}",
            error.message
        );
        let count = captured.lock().unwrap().len();
        assert!(
            count >= 3 && count < 30,
            "retry must back off: {count} requests"
        );
        assert_eq!(entry.phase, Phase::Executing);
        assert!(entry.report.is_none());
    }
}

#[tokio::test]
async fn claim_and_assignment_reads_advertise_runner_capability_without_changing_legacy_payloads() {
    let (_temp, settings, path) = super::tests::fixture();
    let attempt = AttemptStatus {
        assignment: super::tests::assignment(),
        state: "assigned".into(),
        uncertainty_reason: None,
    };
    let (client, captured, server) = tls_script(
        path.parent().unwrap(),
        vec![
            Some((200, json!(null))),
            Some((200, json!([]))),
            Some((200, serde_json::to_value(&attempt).unwrap())),
        ],
    )
    .await;
    assert!(client.claim(&settings).await.unwrap().is_none());
    assert!(client.assignments(&settings).await.unwrap().is_empty());
    assert_eq!(client.attempt("attempt").await.unwrap().state, "assigned");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let observed = captured.lock().unwrap();
    assert_eq!(observed[0].0, "/v1/shared/runner/claim");
    assert_eq!(observed[0].1, json!({"environment_ids":["environment"]}));
    assert_eq!(
        observed[1].0,
        "/v1/shared/runner/assignments?environment_ids=environment"
    );
    assert!(observed[1].1.is_null());
    assert_eq!(observed[2].0, "/v1/shared/runner/attempts/attempt");
}

#[tokio::test]
async fn assignment_poll_names_only_its_mapped_environments_and_preserves_empty_scope() {
    let (_temp, mut settings, path) = super::tests::fixture();
    settings.environments[0].environment_id = "analysis".into();
    let mut second = settings.environments[0].clone();
    second.environment_id = "solver".into();
    settings.environments.push(second);
    let (client, captured, server) = tls_script(
        path.parent().unwrap(),
        vec![Some((200, json!([]))), Some((200, json!([])))],
    )
    .await;
    assert!(client.assignments(&settings).await.unwrap().is_empty());
    settings.environments.clear();
    assert!(client.assignments(&settings).await.unwrap().is_empty());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].0,
        "/v1/shared/runner/assignments?environment_ids=analysis,solver"
    );
    assert_eq!(
        requests[1].0,
        "/v1/shared/runner/assignments?environment_ids="
    );
    assert!(requests.iter().all(|(_, body)| body.is_null()));
}

#[tokio::test]
async fn lost_yield_ack_then_admission_denial_reconciles_the_original_over_real_tls() {
    for (earlier_yield_committed, rejection_status, lose_fallback_ack, legacy_fallback) in [
        (true, 403, false, false),
        (false, 403, false, false),
        (false, 429, false, false),
        (false, 429, true, false),
        (false, 403, false, true),
    ] {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../project_sandbox/runner-shared-tests");
        std::fs::create_dir_all(&base).unwrap();
        let temp = tempfile::tempdir_in(base).unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
        let settings = SharedSettings {
            version: 1,
            hub_id: "hub".into(),
            device_id: "device".into(),
            resource_scope: ResourceScope::WorkspaceIsolation { confirmed: true },
            environments: vec![EnvironmentMapping {
                environment_id: "environment".into(),
                directory: root.clone(),
                access_mode: crate::config::AccessMode::Default,
                allowed_child_environments: vec!["solver".into()],
            }],
        };
        let job = Job {
            id: "job".into(),
            conversation_id: "job".into(),
            root_id: "job".into(),
            parent_id: None,
            project_id: "project".into(),
            environment_id: "environment".into(),
            requestor_id: "user".into(),
            assignee_id: "user".into(),
            title: "Task".into(),
            input: json!({"version":1,"prompt":"Task"}),
            checkpoint: None,
            result: None,
            continued_from: None,
            state: JobState::Running,
            awaiting_child_id: None,
            revision: 2,
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let assignment = Assignment {
            project_context: None,
            attempt_id: "attempt".into(),
            generation: 1,
            authority_generation: 1,
            runner_id: "device".into(),
            stop_requested: false,
            job: job.clone(),
            child_result: None,
            allowed_child_environments: vec!["solver".into()],
            allowed_child_candidates: vec![],
            retained_services: vec![],
            required_runner_capabilities: vec![],
            supports_progress_reports: false,
        };
        let mut reported_job = job.clone();
        reported_job.state = if earlier_yield_committed {
            JobState::WaitingChild
        } else {
            JobState::Failed
        };
        let responses = if earlier_yield_committed {
            vec![
                None,
                Some((rejection_status, json!({"error":"stopped"}))),
                Some((
                    200,
                    serde_json::to_value(AttemptStatus {
                        assignment: Assignment {
                            job: reported_job.clone(),
                            ..assignment.clone()
                        },
                        state: "yielded".into(),
                        uncertainty_reason: None,
                    })
                    .unwrap(),
                )),
            ]
        } else if lose_fallback_ack {
            vec![
                Some((rejection_status, json!({"error":"not admitted"}))),
                Some((403, json!({"error":"revoked"}))),
                None,
                Some((403, json!({"error":"revoked"}))),
                Some((403, json!({"error":"revoked"}))),
                Some((200, serde_json::to_value(reported_job).unwrap())),
            ]
        } else {
            vec![
                Some((rejection_status, json!({"error":"not admitted"}))),
                Some((403, json!({"error":"revoked"}))),
                Some((200, serde_json::to_value(reported_job).unwrap())),
            ]
        };
        let (client, captured, server) = tls_script(&root, responses).await;
        let paths = crate::storage::StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/moyai.sqlite3"),
            truncation_dir: root.join("data/truncation"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let process = crate::app::AppBootstrap::create_process_runtime(
            crate::storage::StoreBundle::new(sqlite),
        )
        .await
        .unwrap();
        let host = RunnerHost::from_process(process).unwrap();
        let mut journal = Journal::open(&root.join("runner.sqlite3"), &settings).unwrap();
        let mut entry = journal
            .intent(assignment, settings.environments[0].clone())
            .unwrap();
        journal.executing(&mut entry).unwrap();
        let original = Report::for_assignment(
            &entry.assignment,
            "outcome",
            ReportOutcome::YieldToChild {
                checkpoint: json!({"receipt":"saved"}),
                child_environment_id: "solver".into(),
                child_title: "Child".into(),
                child_input: json!({"version":1,"prompt":"Solve"}),
                resources_released: true,
            },
        );
        journal.outcome(&mut entry, original.clone()).unwrap();
        let expected_error = if legacy_fallback {
            "Hub did not accept the child handoff".to_string()
        } else {
            format!("Hubへの子仕事の依頼が拒否されました（HTTP {rejection_status}）。")
        };
        if legacy_fallback {
            let saved_fallback = Report::for_assignment(
                &entry.assignment,
                "yield_rejected",
                ReportOutcome::Finished {
                    success: false,
                    result: json!({"version":1,"error":expected_error}),
                    resources_released: true,
                },
            );
            journal
                .retain_rejected_yield_fallback(&mut entry, saved_fallback)
                .unwrap();
            drop(journal);
            journal = Journal::open(&root.join("runner.sqlite3"), &settings).unwrap();
            entry = journal.get("attempt").unwrap().unwrap();
        }
        let mut controller = Controller {
            host: host.clone(),
            settings,
            client,
            journal,
            checkpoint_cursor: String::new(),
            commands: None,
            external: None,
            local_project_id: None,
        };
        if earlier_yield_committed {
            assert!(controller.flush_report(&mut entry).await.is_err());
            assert_eq!(entry.phase, Phase::ReportPending);
            assert_eq!(entry.report, Some(original.clone()));
        }
        let saved_fallback = if lose_fallback_ack {
            assert!(controller.flush_report(&mut entry).await.is_err());
            assert_eq!(entry.phase, Phase::ReportPending);
            assert_eq!(entry.report, Some(original.clone()));
            let saved = entry.fallback_report.clone().unwrap();
            drop(controller.journal);
            controller.journal =
                Journal::open(&root.join("runner.sqlite3"), &controller.settings).unwrap();
            entry = controller.journal.get("attempt").unwrap().unwrap();
            assert_eq!(entry.fallback_report, Some(saved.clone()));
            Some(saved)
        } else {
            entry.fallback_report.clone()
        };
        controller.flush_report(&mut entry).await.unwrap();
        assert_eq!(entry.phase, Phase::Settled);
        assert_eq!(entry.report, Some(original));
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        let observed = captured.lock().unwrap();
        let kinds = observed
            .iter()
            .filter_map(|(_, body)| body["outcome"]["kind"].as_str())
            .collect::<Vec<_>>();
        if earlier_yield_committed {
            assert_eq!(kinds, vec!["yield_to_child", "yield_to_child"]);
            assert!(entry.fallback_report.is_none());
        } else {
            let expected_kinds = if lose_fallback_ack {
                vec!["yield_to_child", "finished", "yield_to_child", "finished"]
            } else {
                vec!["yield_to_child", "finished"]
            };
            assert_eq!(kinds, expected_kinds);
            let fallback = entry.fallback_report.as_ref().unwrap();
            if let Some(saved) = saved_fallback {
                assert_eq!(fallback, &saved);
                assert_eq!(
                    observed.last().unwrap().1,
                    serde_json::to_value(saved).unwrap()
                );
            }
            if lose_fallback_ack {
                assert_eq!(observed[2].1, observed[5].1);
            }
            assert!(matches!(
                &fallback.outcome,
                ReportOutcome::Finished {
                    success: false,
                    result,
                    resources_released: true,
                } if result["error"] == expected_error
            ));
            assert_eq!(
                observed.last().unwrap().1["outcome"]["result"]["error"],
                expected_error
            );
        }
        drop(observed);
        host.begin_shutdown().unwrap();
        host.wait_stopped().await;
    }
}

#[tokio::test]
async fn stop_fence_rejects_late_retention_and_reports_only_after_process_drain() {
    use crate::runner::Execution;
    use crate::runtime::{OwnedTaskHandle, RunControl};
    use tokio_util::sync::CancellationToken;

    let (_temp, settings, path) = super::tests::fixture();
    let root = path.parent().unwrap();
    let mut rejected_job = super::tests::assignment().job;
    rejected_job.state = JobState::Cancelled;
    let (client, captured, server) = tls_script(
        root,
        vec![
            Some((409, json!({"error":"stop fence"}))),
            Some((200, serde_json::to_value(rejected_job).unwrap())),
        ],
    )
    .await;
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/moyai.sqlite3"),
        truncation_dir: root.join("data/truncation"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(super::tests::assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let session = crate::session::SessionId::new();
    let lifetime = CancellationToken::new();
    let shells = host
        .inner
        .process
        .managed_shells()
        .with_lifetime(lifetime.clone(), entry.run_id);
    let service = shells.start_preview_for_test(session).await;
    let worker = OwnedTaskHandle::new(1, tokio::spawn(async {}));
    while !worker.is_finished() {
        tokio::task::yield_now().await;
    }
    host.inner.state.lock().unwrap().runs.insert(
        entry.run_id,
        Execution {
            managed_scope_id: entry.run_id,
            request: LocalRunRequest {
                directory: root.into(),
                prompt: "Retain preview".into(),
                session_id: Some(session),
                title: None,
                single_agent: true,
            },
            session_id: Some(session),
            control: RunControl::new(),
            process_lifetime: lifetime.clone(),
            processes_drained: false,
            service: None,
            worker: Some(worker),
            result: None,
            response: None,
            shared: true,
            resource: None,
        },
    );
    let original = Report::for_assignment(
        &entry.assignment,
        "outcome",
        ReportOutcome::Finished {
            success: true,
            result: json!({"version":1,"text":"preview started"}),
            resources_released: true,
        },
    );
    journal
        .outcome_with_retention(&mut entry, original, None, Some(service))
        .unwrap();
    let mut controller = Controller {
        host: host.clone(),
        settings,
        client,
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    controller.flush_report(&mut entry).await.unwrap();
    assert_eq!(entry.phase, Phase::ReportPending);
    assert!(!entry.retention_reported);
    assert!(!entry.service_stopped_ack);
    assert!(matches!(
        entry.report.as_ref().map(|report| &report.outcome),
        Some(ReportOutcome::Finished { success: false, .. })
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while shells.has_local_work_in_scope(session, entry.run_id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        shells.retained_service_state(service),
        crate::tool::shell::RetainedServiceState::Stopped
    );
    controller.flush_report(&mut entry).await.unwrap();
    assert_eq!(entry.phase, Phase::Settled);
    assert!(entry.service_stopped_ack);
    assert!(lifetime.is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let observed = captured.lock().unwrap();
    assert_eq!(
        observed
            .iter()
            .filter_map(|(_, body)| body["outcome"]["kind"].as_str())
            .collect::<Vec<_>>(),
        vec!["retain_service", "finished"],
        "A rejected retention must not be retried or reported as success"
    );
    drop(observed);
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
}

#[tokio::test]
async fn frozen_terminal_report_survives_service_stop_and_lost_ack_without_payload_rewrite() {
    let (_temp, settings, path) = super::tests::fixture();
    let root = path.parent().unwrap();
    let mut terminal_job = super::tests::assignment().job;
    terminal_job.state = JobState::Cancelled;
    let (client, captured, server) = tls_script(
        root,
        vec![
            None,
            Some((200, serde_json::to_value(terminal_job).unwrap())),
        ],
    )
    .await;
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/moyai.sqlite3"),
        truncation_dir: root.join("data/truncation"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(super::tests::assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let service = crate::tool::shell::RetainedService {
        service_id: ulid::Ulid::new(),
        expires_at_ms: Some(super::super::operations::now_ms() + 60_000),
        retain_after_turn: true,
    };
    let original = Report::for_assignment(
        &entry.assignment,
        "outcome",
        ReportOutcome::Finished {
            success: true,
            result: json!({"version":1,"text":"preview was started"}),
            resources_released: true,
        },
    );
    journal
        .outcome_with_retention(&mut entry, original.clone(), None, Some(service))
        .unwrap();
    journal.retention_reported(&mut entry).unwrap();
    // An archive publication attempt freezes the exact report before the Hub's
    // terminal acknowledgement. Stopping the lease must not replace that report.
    super::data::persist(&client, &host, &entry).await.unwrap();
    assert!(super::data::report_frozen(&host, &entry).unwrap());
    let mut controller = Controller {
        host: host.clone(),
        settings,
        client,
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    controller.flush_report(&mut entry).await.unwrap();
    assert_eq!(entry.phase, Phase::ReportPending);
    assert_eq!(entry.report, Some(original.clone()));
    controller.journal.service_stopped(&mut entry).unwrap();
    assert!(controller.flush_report(&mut entry).await.is_err());
    assert_eq!(entry.report, Some(original.clone()));
    controller.flush_report(&mut entry).await.unwrap();
    assert_eq!(entry.phase, Phase::Settled);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let observed = captured.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].1, observed[1].1);
    assert_eq!(observed[0].1["event_id"], original.event_id);
    drop(observed);
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
}

#[tokio::test]
async fn stopped_yield_keeps_checkpoint_for_canonical_settlement_after_reopen() {
    for cancelled in [false, true] {
        let (_temp, settings, path) = super::tests::fixture();
        let root = path.parent().unwrap();
        let (client, _, server) = tls_script(root, vec![]).await;
        server.await.unwrap();
        let paths = crate::storage::StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/moyai.sqlite3"),
            truncation_dir: root.join("data/truncation"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let process = crate::app::AppBootstrap::create_process_runtime(
            crate::storage::StoreBundle::new(sqlite),
        )
        .await
        .unwrap();
        let host = RunnerHost::from_process(process).unwrap();
        let mut journal = Journal::open(&path, &settings).unwrap();
        let mut entry = journal
            .intent(super::tests::assignment(), settings.environments[0].clone())
            .unwrap();
        journal.executing(&mut entry).unwrap();
        let control = crate::runtime::RunControl::new();
        if cancelled {
            control.cancel(crate::runtime::RunCancellationCause::Interruption(
                crate::protocol::TurnInterruptionCause::UserStop,
            ));
        }
        let worker = crate::runtime::OwnedTaskHandle::new(1, tokio::spawn(async {}));
        while !worker.is_finished() {
            tokio::task::yield_now().await;
        }
        // Model/core finished at its durable Yield boundary. Stop races before the Runner
        // collects that outcome; no worker or process remains to settle the local turn.
        host.inner.state.lock().unwrap().runs.insert(
            entry.run_id,
            super::super::Execution {
                managed_scope_id: entry.run_id,
                request: LocalRunRequest {
                    directory: root.into(),
                    prompt: "Task".into(),
                    session_id: None,
                    title: None,
                    single_agent: true,
                },
                session_id: None,
                control,
                process_lifetime: tokio_util::sync::CancellationToken::new(),
                processes_drained: true,
                shared: true,
                resource: None,
                service: None,
                worker: Some(worker),
                response: None,
                result: Some(Ok(ExecutionOutcome::Yielded(
                    crate::agent::shared::SharedYield {
                        checkpoint: json!({"local_receipt":"durably-paused"}),
                        child: crate::agent::shared::SharedChildRequest {
                            environment_id: "solver".into(),
                            title: "Child".into(),
                            input: json!({"version":1,"prompt":"Solve"}),
                        },
                        session_id: crate::session::SessionId::new(),
                        turn_id: crate::protocol::TurnId::new(),
                        retained_service: None,
                    },
                ))),
            },
        );
        let mut controller = Controller {
            host: host.clone(),
            settings: settings.clone(),
            client,
            journal,
            checkpoint_cursor: String::new(),
            commands: None,
            external: None,
            local_project_id: None,
        };
        controller.collect_outcome(&mut entry).unwrap();
        assert_eq!(entry.phase, Phase::ReportPending);
        assert_eq!(
            matches!(
                entry.report.as_ref().unwrap().outcome,
                ReportOutcome::Finished { success: false, .. }
            ),
            cancelled
        );
        controller.journal.settled(&mut entry).unwrap();
        drop(controller);
        let journal = Journal::open(&path, &settings).unwrap();
        let pending = journal.unsettled_checkpoints("").unwrap();
        host.begin_shutdown().unwrap();
        host.wait_stopped().await;
        assert_eq!(
            pending.len(),
            1,
            "Both a handed-off and a stopped Yield must retain the exact local cleanup receipt after restart"
        );
        assert_eq!(
            pending[0].assignment.attempt_id,
            entry.assignment.attempt_id
        );
        assert_eq!(
            pending[0].checkpoint(),
            Some(&json!({"local_receipt":"durably-paused"}))
        );
    }
}
