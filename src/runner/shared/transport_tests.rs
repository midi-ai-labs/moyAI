use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        for (delay, response) in responses {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(tcp).await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
                assert!(header.len() < 16384);
            }
            let header = String::from_utf8(header).unwrap();
            let path = header.split_whitespace().nth(1).unwrap().to_string();
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
                stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            stream.shutdown().await.unwrap();
        }
    });
    (client, requests, server)
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
    for earlier_yield_committed in [true, false] {
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
            state: JobState::Running,
            awaiting_child_id: None,
            revision: 2,
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let assignment = Assignment {
            attempt_id: "attempt".into(),
            generation: 1,
            authority_generation: 1,
            runner_id: "device".into(),
            stop_requested: false,
            job: job.clone(),
            child_result: None,
            allowed_child_environments: vec!["solver".into()],
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
                Some((403, json!({"error":"stopped"}))),
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
        } else {
            vec![
                Some((403, json!({"error":"revoked"}))),
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
        let mut controller = Controller {
            host: host.clone(),
            settings,
            client,
            journal,
            checkpoint_cursor: String::new(),
            commands: None,
            external: None,
            local_human: None,
        };
        if earlier_yield_committed {
            assert!(controller.flush_report(&mut entry).await.is_err());
            assert_eq!(entry.phase, Phase::ReportPending);
            assert_eq!(entry.report, Some(original.clone()));
        }
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
            assert_eq!(kinds, vec!["yield_to_child", "finished"]);
            assert!(entry.fallback_report.is_some());
        }
        drop(observed);
        host.begin_shutdown().unwrap();
        host.wait_stopped().await;
    }
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
            local_human: None,
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
