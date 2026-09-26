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
async fn local_project_folder_binding_uses_existing_contents_and_reports_current_participation() {
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
            Some((200, json!([{"environment_id":"fresh-env","template_id":"local-folder","generation":1}]))),
            Some((200, json!({"saved":true}))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":false}
            ]))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":false}
            ]))),
            Some((200, json!([{"environment_id":"next-env","template_id":"local-folder","generation":1}]))),
            Some((200, json!({"saved":true}))),
            Some((200, json!({"saved":true}))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":true}
            ]))),
            Some((200, json!({"saved":true}))),
            Some((200, json!([{"environment_id":"next-env","template_id":"local-folder","generation":2,"state":"failed"}]))),
            Some((200, json!([
                {"id":"fresh-env","resource_id":"device-resource","capacity":1,"project_ids":[],"enabled":false},
                {"id":"next-env","resource_id":"device-resource","capacity":1,"project_ids":["project"],"enabled":false}
            ]))),
            Some((200, json!([{"environment_id":"next-env","template_id":"local-folder","generation":2,"state":"failed"}]))),
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
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
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
            "/v1/shared/runner/environments",
            "/v1/shared/runner/provisioning",
            "/v1/shared/runner/provisioning",
            "/v1/shared/runner/environments",
            "/v1/shared/runner/environments",
            "/v1/shared/runner/provisioning",
            "/v1/shared/runner/provisioning",
            "/v1/shared/runner/templates",
            "/v1/shared/runner/environments",
            "/v1/shared/runner/provisioning",
            "/v1/shared/runner/provisioning",
            "/v1/shared/runner/environments",
            "/v1/shared/runner/provisioning",
            "/v1/shared/runner/provisioning",
        ]
    );
    assert_eq!(
        observed[2].1,
        json!({"environment_id":"fresh-env","template_id":"local-folder","generation":1,"success":true,"error":null})
    );
    assert_eq!(
        observed[6].1,
        json!({"environment_id":"next-env","template_id":"local-folder","generation":1,"success":true,"error":null})
    );
    assert_eq!(observed[9].1["success"], false);
    assert_eq!(
        observed[13].1,
        json!({"environment_id":"next-env","template_id":"local-folder","generation":2,"success":true,"error":null})
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
            local_project_id: None,
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
        expires_at_ms: super::super::operations::now_ms() + 60_000,
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
