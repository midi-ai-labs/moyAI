use super::*;
use camino::Utf8PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Script {
    sessions: usize,
    old_hub: bool,
    pause_session: bool,
    unavailable: bool,
    deny: bool,
    projects: Vec<Value>,
    status: Option<Value>,
    requests: Vec<(String, String)>,
}
struct Server {
    shared: crate::device_network::SharedHubConfig,
    script: Arc<Mutex<Script>>,
    session_started: Arc<tokio::sync::Notify>,
    session_release: Arc<tokio::sync::Notify>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn start() -> Self {
        use tokio_rustls::rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
        };
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let ca = params.self_signed(&key).unwrap().pem();
        let issuer = rcgen::Issuer::new(params, key);
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec![
            "127.0.0.1".into(),
            crate::mcp_publish::tls::HUB_TLS_ROLE_NAME.into(),
        ])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(cert.pem().as_bytes()).unwrap()],
                    PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).unwrap(),
                )
                .unwrap(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let shared = crate::device_network::SharedHubConfig {
            hub_url: format!("https://{}", listener.local_addr().unwrap()),
            ca_certificate_pem: ca,
        };
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let script = Arc::new(Mutex::new(Script::default()));
        let requests = script.clone();
        let session_started = Arc::new(tokio::sync::Notify::new());
        let session_release = Arc::new(tokio::sync::Notify::new());
        let started = session_started.clone();
        let release = session_release.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted =
                    tokio::select! {_=cancelled.cancelled()=>break, value=listener.accept()=>value};
                let Ok((tcp, _)) = accepted else { break };
                let operation = async {
                    let mut stream = acceptor.accept(tcp).await.ok()?;
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") {
                        headers.push(stream.read_u8().await.ok()?);
                        if headers.len() > 16384 {
                            return None;
                        }
                    }
                    let headers = String::from_utf8(headers).ok()?;
                    let length = headers
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    assert!(length < 4096);
                    let mut bytes = vec![0; length];
                    stream.read_exact(&mut bytes).await.ok()?;
                    let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
                    let path = headers
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .split('?')
                        .next()
                        .unwrap();
                    let (code, reply) = {
                        let mut script = requests.lock().unwrap();
                        script.requests.push((
                            headers.split_whitespace().next().unwrap().into(),
                            path.into(),
                        ));
                        let principal =
                            json!({"user_id":"Alice","display_name":"Alice","administrator":false});
                        let session = json!({"token":"c".repeat(64),"principal":principal,"expires_at_ms":now_ms()+28_800_000});
                        match path {
                            "/v1/shared/device-session" => {
                                script.sessions += 1;
                                assert_eq!(body, json!({}));
                                assert!(!headers.to_ascii_lowercase().contains("authorization:"));
                                if script.unavailable {
                                    return None;
                                }
                                if script.old_hub {
                                    (404, json!({"error":"not_found"}))
                                } else if script.deny {
                                    (403, json!({"error":"device_identity_required"}))
                                } else {
                                    (200, session)
                                }
                            }
                            "/v1/shared/session" => (200, session),
                            "/v1/shared/projects" => (200, json!(script.projects)),
                            "/v1/shared/status" => {
                                (200, script.status.clone().expect("status fixture"))
                            }
                            "/v1/shared/inbox" => {
                                (200, json!({"items":[],"next_before":null,"unread_count":0}))
                            }
                            _ => panic!("unexpected route {path}"),
                        }
                    };
                    let pause = path == "/v1/shared/device-session"
                        && requests.lock().unwrap().pause_session;
                    if pause {
                        started.notify_one();
                        release.notified().await;
                    }
                    let reply = serde_json::to_vec(&reply).unwrap();
                    stream.write_all(format!("HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).as_bytes()).await.ok()?;
                    stream.write_all(&reply).await.ok()?;
                    let _ = stream.shutdown().await;
                    Some(())
                };
                let _ = tokio::time::timeout(Duration::from_secs(5), operation).await;
            }
        });
        Self {
            shared,
            script,
            session_started,
            session_release,
            stop,
            task,
        }
    }
    async fn stop(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}
async fn service(
    root: &Utf8PathBuf,
    shared: &crate::device_network::SharedHubConfig,
    device: &str,
) -> DeviceNetworkService {
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let paths = StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let service = DeviceNetworkService::for_workspace(
        root.join("config/device"),
        workspace,
        StoreBundle::new(sqlite),
        crate::config::ResolvedConfig::default(),
    )
    .await
    .unwrap();
    {
        let mut state = service.inner.state.lock().unwrap();
        state.settings.hub_id = Some("Hub".into());
        state.settings.device_id = Some(device.into());
        state.shared = shared.clone();
        state.status = "active";
        state.client = Some(DeviceClient::new(shared, None, device.into()).unwrap());
    }
    service
}
async fn command(
    service: &DeviceNetworkService,
    command: SharedWorkCommand,
) -> SharedWorkProjection {
    let generation = service.shared_work_projection().generation;
    service.shared_work_command(&generation, command).await
}
fn expire(service: &DeviceNetworkService) {
    service
        .inner
        .shared_work
        .0
        .lock()
        .unwrap()
        .view
        .expires_at_ms = Some(0);
}

#[tokio::test]
async fn approved_device_authenticates_after_restart_and_renews_without_stored_passwords() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    // Saved credentials from the older human login scheme must not become an input.
    let legacy = root.join("config/device/shared-human-auth.dpapi");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, b"unreadable old credential").unwrap();
    let first = service(&root, &server.shared, "WinA").await;
    let ready = command(&first, SharedWorkCommand::Refresh).await;
    assert_eq!(ready.principal.unwrap().user_id, "Alice");
    drop(first);
    let second = service(&root, &server.shared, "WinA").await;
    let ready = command(&second, SharedWorkCommand::Refresh).await;
    assert_eq!(ready.principal.unwrap().user_id, "Alice");
    assert_eq!(server.script.lock().unwrap().sessions, 2);
    expire(&second);
    server.script.lock().unwrap().unavailable = true;
    let unavailable = command(&second, SharedWorkCommand::Refresh).await;
    assert!(unavailable.principal.is_none());
    assert!(unavailable.error.is_some());
    server.script.lock().unwrap().unavailable = false;
    let restored = command(&second, SharedWorkCommand::Refresh).await;
    assert_eq!(restored.principal.as_ref().unwrap().user_id, "Alice");
    assert!(
        !serde_json::to_string(&restored)
            .unwrap()
            .contains(&"c".repeat(64))
    );
    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        b"unreadable old credential"
    );
    server.stop().await;
}

#[tokio::test]
async fn rejected_device_and_old_hub_never_restore_a_human_or_offer_password_fallback() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    command(&service, SharedWorkCommand::Refresh).await;
    expire(&service);
    server.script.lock().unwrap().deny = true;
    let denied = command(&service, SharedWorkCommand::Refresh).await;
    assert!(denied.principal.is_none());
    assert!(denied.error.unwrap().contains("関連付け"));
    server.script.lock().unwrap().old_hub = true;
    let legacy = command(&service, SharedWorkCommand::Refresh).await;
    assert!(legacy.principal.is_none());
    assert!(legacy.error.unwrap().contains("Hubを更新"));
    assert!(
        server
            .script
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(_, path)| {
                !matches!(
                    path.as_str(),
                    "/v1/shared/login" | "/v1/shared/refresh" | "/v1/shared/setup-password"
                )
            })
    );
    server.stop().await;
}

#[tokio::test]
async fn a_device_change_discards_an_inflight_session_and_old_project_state() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    server.script.lock().unwrap().pause_session = true;
    let refresh = command(&service, SharedWorkCommand::Refresh);
    tokio::pin!(refresh);
    tokio::select! {
        _ = server.session_started.notified() => {},
        _ = &mut refresh => panic!("session should be awaiting its reply"),
    }
    service.inner.state.lock().unwrap().client =
        Some(DeviceClient::new(&server.shared, None, "WinB".into()).unwrap());
    server.session_release.notify_one();
    let stale = refresh.await;
    assert!(stale.principal.is_none());
    assert!(stale.projects.is_empty());
    assert!(service.inner.shared_work.0.lock().unwrap().token.is_none());
    server.stop().await;
}

#[test]
fn remote_management_accepts_only_https_one_use_ticket_urls() {
    let ticket = "a".repeat(64);
    assert!(validate_management_url(&format!("https://hub.test/admin/#access={ticket}")).is_ok());
    for url in [
        format!("http://hub.test/admin/#access={ticket}"),
        format!("https://user:secret@hub.test/admin/#access={ticket}"),
        format!("https://hub.test/admin/?access={ticket}"),
        "https://hub.test/admin/#access=bad".into(),
        "file:///C:/Windows/cmd.exe".into(),
    ] {
        assert!(validate_management_url(&url).is_err());
    }
    assert!(
        serde_json::from_value::<SharedWorkCommand>(
            json!({"kind":"login","username":"old","password":"secret"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<SharedWorkCommand>(json!({"kind":"setup_password","username":"old","password":"secret","code":"a".repeat(64)})).is_err());
}

#[tokio::test]
async fn new_shared_conversation_clears_attachments_only_for_the_current_project_without_submitting()
 {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![
            json!({"id":"project-a","label":"Project A","role":"contributor"}),
            json!({"id":"project-b","label":"Project B","role":"contributor"}),
        ];
        script.status = Some(json!({
            "project_id":"project-a", "jobs":[], "environments":[],
            "next_before":null, "next_environment_before":null,
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    let login = command(&service, SharedWorkCommand::Refresh).await;
    assert_eq!(login.principal.unwrap().user_id, "Alice");
    command(&service, SharedWorkCommand::Refresh).await;
    let selected = command(
        &service,
        SharedWorkCommand::Project {
            project_id: "project-a".into(),
        },
    )
    .await;
    assert!(selected.error.is_none());
    assert_eq!(selected.selected_project_id.as_deref(), Some("project-a"));
    assert_eq!(selected.projects.len(), 2);
    let input: WorkAsset = serde_json::from_value(json!({
        "id":"input-a", "project_id":"project-a", "job_id":null,
        "kind":"input", "name":"draft.txt", "sha256":"a".repeat(64),
        "byte_length":12, "created_at_ms":1, "version":1, "base_sha256":null,
    }))
    .unwrap();
    {
        // Prior selections/attachment metadata are browser-independent runtime state.
        // Native file selection and Hub upload have their own tests; this case targets
        // the public NewConversation command, with real authentication/refresh traffic.
        let mut runtime = service.inner.shared_work.0.lock().unwrap();
        runtime.view.selected_job_id = Some("job-a".into());
        runtime.view.detail = Some(
            serde_json::from_value(json!({
                "id":"job-a", "project_id":"project-a", "root_id":"job-a",
                "parent_id":null, "environment_id":"env-a", "title":"Previous conversation",
                "input":{"version":1,"prompt":"Prior request"}, "result":{"text":"Done"},
                "state":"succeeded", "awaiting_child_id":null, "revision":2,
                "created_at_ms":1, "updated_at_ms":2,
            }))
            .unwrap(),
        );
        runtime.view.inputs.push(input.clone());
        runtime.view.assets.push(input.clone());
        runtime.view.transcript = Some(WorkTranscript {
            items: vec![],
            next_after: Some(100),
        });
        runtime.transcript_after = 100;
    }
    let request_count = server.script.lock().unwrap().requests.len();
    let wrong = command(
        &service,
        SharedWorkCommand::NewConversation {
            project_id: "project-b".into(),
        },
    )
    .await;
    assert!(wrong.error.is_some());
    assert_eq!(wrong.selected_project_id.as_deref(), Some("project-a"));
    assert_eq!(wrong.selected_job_id.as_deref(), Some("job-a"));
    assert_eq!(wrong.detail.as_ref().unwrap().id, "job-a");
    assert_eq!(serde_json::to_value(&wrong.inputs).unwrap(), json!([input]));
    assert!(wrong.transcript.is_some());
    assert_eq!(server.script.lock().unwrap().requests.len(), request_count);

    let fresh = command(
        &service,
        SharedWorkCommand::NewConversation {
            project_id: "project-a".into(),
        },
    )
    .await;
    assert!(fresh.error.is_none(), "{:?}", fresh.error);
    assert_eq!(fresh.principal.as_ref().unwrap().user_id, "Alice");
    assert_eq!(fresh.selected_project_id.as_deref(), Some("project-a"));
    assert_eq!(fresh.projects.len(), 2);
    assert_eq!(fresh.status.as_ref().unwrap().project_id, "project-a");
    assert!(fresh.selected_job_id.is_none());
    assert!(fresh.detail.is_none());
    assert!(fresh.inputs.is_empty());
    assert!(fresh.assets.is_empty());
    assert!(fresh.transcript.is_none());
    assert_eq!(
        service.inner.shared_work.0.lock().unwrap().transcript_after,
        0
    );
    {
        let script = server.script.lock().unwrap();
        assert_eq!(script.sessions, 1);
        assert!(script.requests.len() > request_count);
        assert!(
            script.requests[request_count..]
                .iter()
                .all(|(method, _)| method == "GET")
        );
        assert!(
            script.requests[request_count..]
                .iter()
                .any(|(_, path)| path == "/v1/shared/status")
        );
    }
    drop(service);
    server.stop().await;
}
