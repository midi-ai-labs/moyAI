use super::*;
use base64::Engine;
use camino::Utf8PathBuf;
use sha2::Digest;
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
    device_projects: Option<Vec<Value>>,
    environments: Vec<Value>,
    job: Option<Value>,
    child_job: Option<Value>,
    newer_job: Option<Value>,
    child_approval: Option<Value>,
    inbox_items: Vec<Value>,
    approval_decisions: Vec<Value>,
    submitted_job: Option<Value>,
    submit_requests: Vec<Value>,
    submit_once_unavailable: bool,
    submit_once_conflict: bool,
    asset: Option<Value>,
    uploaded_asset: Option<Value>,
    artifacts: Vec<Value>,
    downloaded_artifact: Option<Value>,
    status: Option<Value>,
    stop_requests: Vec<Value>,
    stop_once_unavailable: bool,
    leave_requests: Vec<Value>,
    leave_once_unavailable: bool,
    origin_stop_requests: Vec<Value>,
    origin_stop_once_unavailable: bool,
    origin_all_stop_requests: Vec<Value>,
    rename_requests: Vec<Value>,
    delete_requests: Vec<Value>,
    conversation_revision: u64,
    conversation_latest_job_id: Option<String>,
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
                            "/v1/shared/device-projects" => {
                                let projects = script.device_projects.clone().unwrap_or_else(|| {
                                    script.projects.iter().map(|project| json!({
                                        "id":project["id"],"label":project["label"],
                                        "can_control":true,"can_execute":true,
                                        "participation_generation":project["participation_generation"],
                                        "environment_id":"env-a","preparation_state":"ready","error":null
                                    })).collect()
                                });
                                (200, json!({"projects":projects}))
                            }
                            "/v1/shared/projects/project-a/leave" => {
                                script.leave_requests.push(body.clone());
                                if script.leave_once_unavailable {
                                    script.leave_once_unavailable = false;
                                    (503, json!({"error":"temporarily_unavailable"}))
                                } else {
                                    script
                                        .projects
                                        .retain(|project| project["id"] != "project-a");
                                    if let Some(projects) = &mut script.device_projects {
                                        projects.retain(|project| project["id"] != "project-a");
                                    }
                                    (
                                        200,
                                        json!({"left":true,"project_id":"project-a",
                                        "participation_generation":body["expected_participation_generation"]}),
                                    )
                                }
                            }
                            path if path.starts_with("/v1/shared/projects/")
                                && path.ends_with("/conversations") =>
                            {
                                let project = path
                                    .trim_start_matches("/v1/shared/projects/")
                                    .trim_end_matches("/conversations");
                                let conversations = script.job.as_ref()
                                    .filter(|job| job["project_id"] == project)
                                    .map(|job| vec![json!({
                                        "id":job["conversation_id"],
                                        "title":job["title"],
                                        "latest_job_id":script.conversation_latest_job_id.as_ref()
                                            .map(|id| json!(id)).unwrap_or_else(|| job["id"].clone()),
                                        "updated_at_ms":job["updated_at_ms"],
                                        "revision":script.conversation_revision.max(1),
                                        "delete_pending":false,
                                        "can_rename":true,
                                        "can_delete":true,
                                        "can_revise":job["can_revise"].as_bool().unwrap_or(false)
                                    })])
                                    .unwrap_or_default();
                                (200, json!({"revision":"1","conversations":conversations}))
                            }
                            "/v1/shared/environments" => (200, json!(script.environments)),
                            "/v1/shared/origins/local-session-a/turns/turn-a/admit" => {
                                assert_eq!(
                                    body,
                                    json!({"request_id":"admit-turn-a", "create":true,
                                    "origin_turn_revision":1})
                                );
                                (
                                    200,
                                    json!({"origin_session_ref":"local-session-a",
                                    "origin_turn_ref":"turn-a","epoch":1}),
                                )
                            }
                            "/v1/shared/origins/local-session-a/turns/turn-a/stop" => {
                                script.origin_stop_requests.push(body.clone());
                                if script.origin_stop_once_unavailable {
                                    script.origin_stop_once_unavailable = false;
                                    (503, json!({"error":"temporarily_unavailable"}))
                                } else {
                                    (
                                        200,
                                        json!({"accepted":true,
                                        "origin_session_ref":"local-session-a",
                                        "origin_turn_ref":"turn-a",
                                        "request_id":body["request_id"]}),
                                    )
                                }
                            }
                            "/v1/shared/origins/local-session-a/stop" => {
                                script.origin_all_stop_requests.push(body.clone());
                                (
                                    200,
                                    json!({"accepted":true,
                                    "origin_session_ref":"local-session-a",
                                    "origin_turn_ref":null,
                                    "request_id":body["request_id"]}),
                                )
                            }
                            "/v1/shared/jobs" => {
                                script.submit_requests.push(body.clone());
                                script.submitted_job = Some(body);
                                if script.submitted_job.as_ref().unwrap()["revises_job_id"]
                                    == "job-a"
                                {
                                    let mut revised = script.job.clone().expect("job fixture");
                                    revised["id"] = json!("job-revised");
                                    revised["conversation_id"] = json!("job-a");
                                    revised["root_id"] = json!("job-revised");
                                    revised["title"] =
                                        script.submitted_job.as_ref().unwrap()["title"].clone();
                                    revised["input"] =
                                        script.submitted_job.as_ref().unwrap()["input"].clone();
                                    revised["revises_job_id"] = json!("job-a");
                                    revised["can_revise"] = json!(false);
                                    script.job = Some(revised);
                                }
                                if script.submit_once_unavailable {
                                    script.submit_once_unavailable = false;
                                    (503, json!({"error":"temporarily_unavailable"}))
                                } else if script.submit_once_conflict {
                                    script.submit_once_conflict = false;
                                    (409, json!({"error":"origin_turn_stopped"}))
                                } else {
                                    (200, script.job.clone().expect("job fixture"))
                                }
                            }
                            "/v1/shared/jobs/job-a" => {
                                (200, script.job.clone().expect("job fixture"))
                            }
                            "/v1/shared/jobs/job-child" => {
                                (200, script.child_job.clone().expect("child job fixture"))
                            }
                            "/v1/shared/jobs/job-revised" => (
                                200,
                                script
                                    .newer_job
                                    .clone()
                                    .or_else(|| script.job.clone())
                                    .expect("job fixture"),
                            ),
                            "/v1/shared/jobs/job-a/assets" => (200, json!(script.artifacts)),
                            "/v1/shared/jobs/job-child/assets" => (200, json!([])),
                            "/v1/shared/jobs/job-revised/assets" => (200, json!(script.artifacts)),
                            "/v1/shared/jobs/job-a/transcript" => {
                                (200, json!({"items":[],"next_after":null}))
                            }
                            "/v1/shared/jobs/job-child/transcript" => {
                                (200, json!({"items":[],"next_after":null}))
                            }
                            "/v1/shared/jobs/job-revised/transcript" => {
                                (200, json!({"items":[],"next_after":null}))
                            }
                            "/v1/shared/jobs/job-a/handover" => (
                                200,
                                json!({"candidates":[],"pending":null,"can_handover":false}),
                            ),
                            "/v1/shared/jobs/job-child/handover" => (
                                200,
                                json!({"candidates":[],"pending":null,"can_handover":false}),
                            ),
                            "/v1/shared/jobs/job-revised/handover" => (
                                200,
                                json!({"candidates":[],"pending":null,"can_handover":false}),
                            ),
                            "/v1/shared/jobs/job-a/approval" => (200, Value::Null),
                            "/v1/shared/jobs/job-child/approval" => (
                                200,
                                script
                                    .child_approval
                                    .clone()
                                    .expect("child approval fixture"),
                            ),
                            "/v1/shared/jobs/job-child/approvals/approval-child/decision" => {
                                script.approval_decisions.push(body.clone());
                                let approval = script
                                    .child_approval
                                    .as_mut()
                                    .expect("child approval fixture");
                                approval["status"] = json!("decided");
                                approval["decision"] = body["decision"].clone();
                                approval["can_decide"] = json!(false);
                                (200, approval.clone())
                            }
                            "/v1/shared/jobs/job-revised/approval" => (200, Value::Null),
                            "/v1/shared/conversations/job-a/history" => {
                                (404, json!({"error":"not_found"}))
                            }
                            "/v1/shared/conversations/job-a/rename" => {
                                script.rename_requests.push(body.clone());
                                let current = script.conversation_revision.max(1);
                                if body["expected_revision"] != current {
                                    (409, json!({"error":"revision_conflict"}))
                                } else {
                                    script.conversation_revision = current + 1;
                                    if let Some(job) = script.job.as_mut() {
                                        job["title"] = body["title"].clone();
                                    }
                                    (
                                        200,
                                        json!({"id":"job-a","title":body["title"],"revision":script.conversation_revision}),
                                    )
                                }
                            }
                            "/v1/shared/conversations/job-a/delete" => {
                                script.delete_requests.push(body.clone());
                                let current = script.conversation_revision.max(1);
                                if body["expected_revision"] != current {
                                    (409, json!({"error":"revision_conflict"}))
                                } else {
                                    script.job = None;
                                    (
                                        200,
                                        json!({"id":"job-a","delete_pending":false,"deleted":true}),
                                    )
                                }
                            }
                            "/v1/shared/assets/output-a" => (
                                200,
                                script
                                    .downloaded_artifact
                                    .clone()
                                    .expect("download fixture"),
                            ),
                            "/v1/shared/projects/project-a/assets" => {
                                script.uploaded_asset = Some(body);
                                (200, script.asset.clone().expect("asset fixture"))
                            }
                            "/v1/shared/status" => {
                                (200, script.status.clone().expect("status fixture"))
                            }
                            "/v1/shared/conversations/conversation-a/services/stop" => {
                                script.stop_requests.push(body.clone());
                                if script.stop_once_unavailable {
                                    script.stop_once_unavailable = false;
                                    (503, json!({"error":"temporarily_unavailable"}))
                                } else {
                                    (
                                        200,
                                        json!({"project_id":body["project_id"],
                                        "conversation_id":"conversation-a",
                                        "request_id":body["request_id"],"accepted":true}),
                                    )
                                }
                            }
                            "/v1/shared/inbox" => (
                                200,
                                json!({"items":script.inbox_items,"next_before":null,"unread_count":script.inbox_items.len()}),
                            ),
                            "/v1/shared/inbox/notification-child/read" => {
                                (200, json!({"read":true}))
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

#[tokio::test]
async fn stopped_origin_rejects_late_submit_without_leaving_an_uncertain_receipt() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({"id":"project-a","label":"Team",
            "role":"contributor","can_submit":true})];
        script.environments = vec![json!({"id":"env-b","label":"WinB",
            "device_id":"device-b","enabled":true,"can_submit":true,
            "capacity":1,"occupied":0,"capabilities":[]})];
        script.submit_once_conflict = true;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    let control = crate::runtime::RunControl::new();
    let error = service
        .agent_submit_job(
            "late-call",
            "local-session-a",
            "turn-a",
            1,
            "project-a",
            "env-b",
            "Late",
            "Build it",
            &[],
            || Ok(control.begin_tool_effect_commit().unwrap()),
        )
        .await
        .unwrap_err();
    assert!(error.contains("状態が変わりました"));
    assert!(
        !service
            .origin_has_pending_submission("local-session-a")
            .unwrap()
    );
    server.stop().await;
}

#[tokio::test]
async fn exact_origin_stop_survives_lost_reply_and_coexists_with_uncertain_submission() {
    let server = Server::start().await;
    server.script.lock().unwrap().origin_stop_once_unavailable = true;
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let first = service(&root, &server.shared, "WinA").await;
    // An uncertain submit must not prevent the exact Stop from being saved.
    let (connection, session) = first.agent_session().await.unwrap();
    first
        .inner
        .shared_work
        .0
        .lock()
        .unwrap()
        .receipts
        .insert(Receipt {
            hub: connection.hub_binding.clone(),
            user_id: session.principal.user_id,
            operation: ReceiptOperation::Submit,
            payload: json!({"request_id":"pending-a","origin_session_ref":"local-session-a",
            "origin_turn_ref":"turn-a","origin_turn_epoch":1,
            "project_id":"project-a","environment_id":"env-b","title":"TODO app",
            "input":{"version":2,"prompt":"Build it","input_refs":[]},
            "descendant_budget":8}),
        })
        .unwrap();
    assert!(
        first
            .queue_origin_turn_stop("local-session-a", "turn-a", 1)
            .unwrap()
    );
    assert!(
        first
            .queue_origin_conversation_stop("local-session-a", 1)
            .unwrap()
    );
    assert!(
        first
            .agent_retry_origin_turn_stops()
            .await
            .unwrap_err()
            .contains("saved")
    );
    drop(first);

    let restored = service(&root, &server.shared, "WinA").await;
    assert!(
        restored
            .queue_origin_turn_stop("local-session-a", "turn-a", 1)
            .unwrap()
    );
    assert!(
        restored
            .queue_origin_turn_stop("local-session-a", "turn-a", 2)
            .is_err()
    );
    restored.agent_retry_origin_turn_stops().await.unwrap();
    let script = server.script.lock().unwrap();
    assert_eq!(script.origin_stop_requests.len(), 2);
    assert_eq!(
        script.origin_stop_requests[0],
        script.origin_stop_requests[1]
    );
    assert_eq!(
        script.origin_stop_requests[0],
        json!({"request_id":"stop-turn-a",
        "origin_turn_revision":1})
    );
    assert_eq!(script.origin_all_stop_requests.len(), 1);
    assert_eq!(script.origin_all_stop_requests[0]["through_revision"], 1);
    drop(script);
    assert!(
        restored
            .origin_has_pending_submission("local-session-a")
            .unwrap()
    );
    server.stop().await;
}

#[tokio::test]
async fn prepared_origin_stop_is_never_sent_before_local_user_stop_commit() {
    let server = Server::start().await;
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let origin = crate::session::SessionId::new().to_string();
    let turn = crate::protocol::TurnId::new().to_string();
    let first = service(&root, &server.shared, "WinA").await;
    assert!(first.prepare_origin_turn_stop(&origin, &turn, 1).unwrap());
    first.agent_retry_origin_turn_stops().await.unwrap();
    assert!(
        server
            .script
            .lock()
            .unwrap()
            .origin_stop_requests
            .is_empty()
    );
    drop(first);
    let restored = service(&root, &server.shared, "WinA").await;
    restored.agent_retry_origin_turn_stops().await.unwrap();
    assert!(
        server
            .script
            .lock()
            .unwrap()
            .origin_stop_requests
            .is_empty()
    );
    let binding = restored.agent_session().await.unwrap().0.hub_binding;
    assert!(
        restored
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .pending_origin_stop(&binding, &origin, &turn)
            .is_some()
    );
    server.stop().await;
}

#[tokio::test]
async fn conversation_stop_retries_the_saved_request_id_after_unknown_reply() {
    let server = Server::start().await;
    server.script.lock().unwrap().stop_once_unavailable = true;
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    let control = crate::runtime::RunControl::new();
    let first = service
        .agent_stop_conversation("stop-first", "project-a", "conversation-a", || {
            Ok(control.begin_tool_effect_commit().unwrap())
        })
        .await;
    assert!(first.unwrap_err().contains("stop request ID is saved"));
    let acknowledged = service
        .agent_stop_conversation("stop-second", "project-a", "conversation-a", || {
            Ok(control.begin_tool_effect_commit().unwrap())
        })
        .await
        .unwrap();
    assert_eq!(acknowledged["request_id"], "stop-first");
    let script = server.script.lock().unwrap();
    assert_eq!(script.stop_requests.len(), 2);
    assert_eq!(script.stop_requests[0], script.stop_requests[1]);
    drop(script);
    server.stop().await;
}

#[tokio::test]
async fn ordinary_chat_retries_only_its_durable_uncertain_submission_after_restart() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects =
            vec![json!({"id":"project-a","label":"Team","role":"contributor","can_submit":true})];
        script.environments = vec![json!({"id":"env-b","label":"WinB","device_id":"device-b",
            "device_label":"WinB","enabled":true,"can_submit":true,"capacity":1,"occupied":0,
            "capabilities":[]})];
        script.job = Some(json!({"id":"job-a","conversation_id":"job-a",
            "origin_session_ref":"local-session-a","origin_turn_ref":"turn-a",
            "origin_turn_epoch":1,"project_id":"project-a","root_id":"job-a",
            "parent_id":null,"environment_id":"env-b","title":"TODO app",
            "input":{"version":2,"prompt":"Build a TODO app","input_refs":[]},
            "result":null,"state":"queued","awaiting_child_id":null,
            "retained_services":[],"revision":1,"created_at_ms":1,"updated_at_ms":1}));
        script.submit_once_unavailable = true;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let first = service(&root, &server.shared, "WinA").await;
    let control = crate::runtime::RunControl::new();
    let unknown = first
        .agent_submit_job(
            "original-call",
            "local-session-a",
            "turn-a",
            1,
            "project-a",
            "env-b",
            "TODO app",
            "Build a TODO app",
            &[],
            || Ok(control.begin_tool_effect_commit().unwrap()),
        )
        .await;
    assert!(unknown.unwrap_err().contains("receipt"));
    assert!(
        first
            .origin_has_pending_submission("local-session-a")
            .unwrap()
    );
    assert!(
        !first
            .origin_has_pending_submission("another-session")
            .unwrap()
    );
    drop(first);

    let restored = service(&root, &server.shared, "WinA").await;
    assert!(
        restored
            .origin_has_pending_submission("local-session-a")
            .unwrap()
    );
    let wrong_chat = restored
        .agent_retry_submission("another-session", || {
            Ok(control.begin_tool_effect_commit().unwrap())
        })
        .await;
    assert!(wrong_chat.is_err());
    assert_eq!(server.script.lock().unwrap().submit_requests.len(), 1);
    server.script.lock().unwrap().job.as_mut().unwrap()["environment_id"] = json!("other-env");
    assert!(
        restored
            .agent_retry_submission("local-session-a", || {
                Ok(control.begin_tool_effect_commit().unwrap())
            })
            .await
            .unwrap_err()
            .contains("different saved submission")
    );
    assert!(
        restored
            .origin_has_pending_submission("local-session-a")
            .unwrap()
    );
    server.script.lock().unwrap().job.as_mut().unwrap()["environment_id"] = json!("env-b");
    server.script.lock().unwrap().job.as_mut().unwrap()["title"] = json!("different title");
    assert!(
        restored
            .agent_retry_submission("local-session-a", || {
                Ok(control.begin_tool_effect_commit().unwrap())
            })
            .await
            .unwrap_err()
            .contains("different saved submission")
    );
    assert!(
        restored
            .origin_has_pending_submission("local-session-a")
            .unwrap()
    );
    server.script.lock().unwrap().job.as_mut().unwrap()["title"] = json!("TODO app");
    let recovered = restored
        .agent_retry_submission("local-session-a", || {
            Ok(control.begin_tool_effect_commit().unwrap())
        })
        .await
        .unwrap();
    assert_eq!(recovered["job_id"], "job-a");
    assert_eq!(recovered["recovered"], true);
    {
        let script = server.script.lock().unwrap();
        assert_eq!(script.submit_requests.len(), 4);
        assert_eq!(script.submit_requests[0], script.submit_requests[1]);
        assert_eq!(script.submit_requests[1], script.submit_requests[2]);
        assert_eq!(script.submit_requests[2], script.submit_requests[3]);
        assert_eq!(script.submit_requests[3]["request_id"], "original-call");
    }
    assert!(
        !restored
            .origin_has_pending_submission("local-session-a")
            .unwrap()
    );
    assert!(
        restored
            .agent_retry_submission("local-session-a", || {
                Ok(control.begin_tool_effect_commit().unwrap())
            })
            .await
            .is_err()
    );
    server.stop().await;
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

#[tokio::test]
async fn team_save_artifact_writes_verified_binary_without_leaving_or_clobbering_workspace() {
    use crate::cli::ConfirmationPrompt;
    use crate::config::{AccessMode, ResolvedConfig};
    use crate::edit::{ChangeTracker, EditSafety, Formatter};
    use crate::protocol::{ReviewDecision, TurnId};
    use crate::session::{
        NewSession, ProjectRepository, SessionContext, SessionRepository, ToolCallId,
    };
    use crate::tool::context::{RunMutationFence, ToolContext, ToolServices};
    use crate::tool::registry::Tool;
    use crate::tool::team_work::TeamSaveArtifactTool;
    use crate::tool::truncate::ToolTruncator;
    use crate::workspace::WorkspaceDiscovery;

    struct UnexpectedPrompt;
    impl ConfirmationPrompt for UnexpectedPrompt {
        fn confirm(
            &mut self,
            _: &crate::tool::PermissionRequest,
        ) -> Result<ReviewDecision, crate::error::CliPromptError> {
            panic!("full-access fixture must not prompt")
        }
    }

    async fn save(
        session: &SessionContext,
        config: &ResolvedConfig,
        services: &ToolServices,
        control: &crate::runtime::RunControl,
        fence: &RunMutationFence,
        path: &str,
    ) -> Result<crate::tool::ToolResult, crate::error::ToolError> {
        let mut prompt = UnexpectedPrompt;
        TeamSaveArtifactTool
            .execute(
                json!({"project_id":"project-a","job_id":"job-a",
                    "asset_id":"output-a","path":path}),
                ToolContext {
                    session,
                    workspace: &session.workspace,
                    config,
                    tool_call_id: ToolCallId::new(),
                    cancel: control.token(),
                    run_control: control.clone(),
                    run_mutation_fence: fence.clone(),
                    prompt: &mut prompt,
                    services,
                    agent: None,
                    permission_guardian: None,
                },
            )
            .await
    }

    let server = Server::start().await;
    let binary = vec![0xa5; 100 * 1024];
    let sha256 = format!("{:x}", sha2::Sha256::digest(&binary));
    {
        let mut script = server.script.lock().unwrap();
        script.job = Some(json!({"id":"job-a","conversation_id":"job-a",
            "origin_session_ref":"local-session-a","project_id":"project-a",
            "root_id":"job-a","parent_id":null,"environment_id":"env-b",
            "title":"Binary output","input":{"version":2,"prompt":"Create output","input_refs":[]},
            "result":{"version":1,"text":"Done"},"state":"succeeded",
            "awaiting_child_id":null,"retained_services":[],"revision":2,
            "created_at_ms":1,"updated_at_ms":2}));
        let asset = json!({"id":"output-a","project_id":"project-a","job_id":"job-a",
            "kind":"artifact","name":"output.bin","sha256":sha256,
            "byte_length":binary.len(),"created_at_ms":2,"version":1,
            "base_sha256":null,"purged_at_ms":null});
        script.artifacts = vec![asset.clone()];
        script.downloaded_artifact = Some(json!({"asset":asset,
            "content_base64":base64::engine::general_purpose::STANDARD.encode(&binary)}));
    }
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let network = service(&root, &server.shared, "WinA").await;
    let store = network.inner.store.clone();
    let workspace_root = root.join("workspace");
    let project_id = store
        .project_repo()
        .list_projects(10)
        .await
        .unwrap()
        .into_iter()
        .find(|project| project.root_path == workspace_root)
        .expect("device service registered the selected workspace")
        .id;
    let mut config = ResolvedConfig::default();
    config.permissions.access_mode = AccessMode::FullAccess;
    let session_record = store
        .session_repo()
        .create_session(NewSession {
            project_id,
            title: "artifact fixture".into(),
            cwd: workspace_root.clone(),
            model: "model".into(),
            base_url: "http://localhost:1234".into(),
            access_mode: AccessMode::FullAccess,
            provider_connection: None,
        })
        .await
        .unwrap();
    let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config).unwrap();
    let session = SessionContext {
        session: session_record,
        workspace,
    };
    let services = ToolServices {
        edit_safety: EditSafety::default(),
        formatter: Formatter::new(config.format.clone()),
        change_tracker: ChangeTracker,
        store: store.clone(),
        storage_paths: store.paths().clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
        managed_shells: Default::default(),
    };
    let turn_id = TurnId::new();
    let admission_id = store
        .session_repo()
        .admit_session_turn(session.session.id, turn_id)
        .await
        .unwrap()
        .unwrap()
        .admission_id;
    let control = crate::runtime::RunControl::new();
    let fence = RunMutationFence::new(
        store.session_repo(),
        session.session.id,
        admission_id,
        turn_id,
        control.clone(),
    );

    let result = save(&session, &config, &services, &control, &fence, "output.bin")
        .await
        .unwrap();
    let saved = workspace_root.join("output.bin");
    let saved_bytes = std::fs::read(&saved).unwrap();
    assert_eq!(saved_bytes, binary);
    assert_eq!(format!("{:x}", sha2::Sha256::digest(&saved_bytes)), sha256);
    assert_eq!(result.metadata["saved"], true);
    assert_eq!(result.metadata["path"], saved.as_str());

    let existing = workspace_root.join("existing.bin");
    std::fs::write(&existing, b"external").unwrap();
    assert!(
        save(
            &session,
            &config,
            &services,
            &control,
            &fence,
            "existing.bin"
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"external");

    let outside = root.join("outside.bin");
    assert!(
        save(
            &session,
            &config,
            &services,
            &control,
            &fence,
            outside.as_str()
        )
        .await
        .is_err()
    );
    assert!(!outside.exists());
    server.stop().await;
}
async fn command(
    service: &DeviceNetworkService,
    command: SharedWorkCommand,
) -> SharedWorkProjection {
    let generation = service.shared_work_projection().generation;
    service.shared_work_command(&generation, command).await
}
#[tokio::test]
async fn shared_project_chat_submits_current_participation_without_manual_pc_choice() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"TODO app","role":"contributor",
            "can_submit":true,"participation_generation":3
        })];
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
        script.job = Some(json!({
            "id":"job-a","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":null,"environment_id":"env-a",
            "title":"WinB hosts a TODO app","input":{"version":2,"prompt":"WinB hosts a Flask TODO app","input_refs":[]},
            "result":null,"state":"queued","awaiting_child_id":null,
            "revision":1,"created_at_ms":1,"updated_at_ms":1
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    let loaded = command(&service, SharedWorkCommand::Refresh).await;
    assert!(loaded.error.is_none(), "{:?}", loaded.error);
    let sent = command(
        &service,
        SharedWorkCommand::Submit {
            project_id: "project-a".into(),
            title: String::new(),
            prompt: "WinB hosts a Flask TODO app".into(),
            start_before_ms: None,
        },
    )
    .await;
    assert!(
        sent.error.is_none(),
        "{:?}; requests={:?}",
        sent.error,
        server.script.lock().unwrap().requests
    );
    assert_eq!(sent.selected_conversation_id.as_deref(), Some("job-a"));
    let script = server.script.lock().unwrap();
    assert_eq!(script.submit_requests.len(), 1);
    let submitted = &script.submit_requests[0];
    assert_eq!(submitted["environment_id"], "");
    assert_eq!(submitted["project_participation"], 3);
    assert_eq!(submitted["title"], "WinB hosts a Flask TODO app");
    drop(script);
    server.stop().await;
}
#[tokio::test]
async fn child_approval_notification_keeps_exact_job_selected_until_decision_or_new_root() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"Team","role":"contributor",
            "can_submit":true,"participation_generation":1
        })];
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
        script.job = Some(json!({
            "id":"job-a","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":null,"environment_id":"env-a",
            "title":"Build the app","input":{"prompt":"Build the app"},
            "result":null,"state":"waiting_child","awaiting_child_id":"job-child",
            "revision":1,"created_at_ms":1,"updated_at_ms":1
        }));
        script.child_job = Some(json!({
            "id":"job-child","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":"job-a","environment_id":"env-b",
            "title":"Host the app","input":{"prompt":"Host the app"},
            "result":null,"state":"running","awaiting_child_id":null,
            "revision":1,"created_at_ms":2,"updated_at_ms":2
        }));
        script.child_approval = Some(json!({
            "id":"approval-child","attempt_id":"attempt-child","status":"pending",
            "decision":null,"expires_at_ms":now_ms()+300_000,"can_decide":true,
            "request":{"access":"shell","summary":"Run a shell check",
                "details":["Command: Get-ChildItem -LiteralPath ."],"targets":["C:/workspace"],
                "outside_workspace":false,"risks":["unclassified_shell"]}
        }));
        script.inbox_items = vec![json!({
            "id":"notification-child","job_id":"job-child","project_id":"project-a",
            "kind":"approval","title":"Host the app","created_at_ms":2,
            "read_at_ms":null,"can_act":true,"approval_id":"approval-child",
            "approval_status":"pending","approval_decision":null
        })];
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    assert!(
        command(&service, SharedWorkCommand::Refresh)
            .await
            .error
            .is_none()
    );
    let parent = command(
        &service,
        SharedWorkCommand::SelectConversation {
            project_id: "project-a".into(),
            conversation_id: "job-a".into(),
        },
    )
    .await;
    assert_eq!(parent.selected_job_id.as_deref(), Some("job-a"));
    let notification = &parent.inbox.as_ref().unwrap().items[0];
    assert_eq!(notification.approval_status.as_deref(), Some("pending"));
    assert_eq!(notification.approval_decision, None);

    let child = command(
        &service,
        SharedWorkCommand::InboxOpen {
            notification_id: "notification-child".into(),
        },
    )
    .await;
    assert!(child.error.is_none(), "{:?}", child.error);
    assert_eq!(child.selected_job_id.as_deref(), Some("job-child"));
    assert_eq!(child.selected_conversation_id.as_deref(), Some("job-a"));
    assert_eq!(
        child.approval.as_ref().map(|approval| approval.id.as_str()),
        Some("approval-child")
    );
    for _ in 0..2 {
        let polled = command(&service, SharedWorkCommand::Refresh).await;
        assert!(polled.error.is_none(), "{:?}", polled.error);
        assert_eq!(polled.selected_job_id.as_deref(), Some("job-child"));
        assert!(
            polled
                .approval
                .as_ref()
                .is_some_and(|approval| approval.can_decide)
        );
    }
    let decided = command(
        &service,
        SharedWorkCommand::Decide {
            project_id: "project-a".into(),
            job_id: "job-child".into(),
            approval_id: "approval-child".into(),
            decision: WorkDecision::Approve,
        },
    )
    .await;
    assert!(decided.error.is_none(), "{:?}", decided.error);
    assert_eq!(decided.selected_job_id.as_deref(), Some("job-child"));
    assert_eq!(
        decided
            .approval
            .as_ref()
            .and_then(|approval| approval.decision.as_deref()),
        Some("approve")
    );
    assert_eq!(
        server.script.lock().unwrap().approval_decisions,
        vec![json!({"decision":"approve"})]
    );

    let parent = command(
        &service,
        SharedWorkCommand::SelectConversation {
            project_id: "project-a".into(),
            conversation_id: "job-a".into(),
        },
    )
    .await;
    assert_eq!(parent.selected_job_id.as_deref(), Some("job-a"));
    let child = command(
        &service,
        SharedWorkCommand::InboxOpen {
            notification_id: "notification-child".into(),
        },
    )
    .await;
    assert_eq!(child.selected_job_id.as_deref(), Some("job-child"));
    {
        let mut script = server.script.lock().unwrap();
        script.conversation_latest_job_id = Some("job-revised".into());
        script.newer_job = Some(json!({
            "id":"job-revised","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-revised","parent_id":null,"environment_id":"env-a",
            "title":"New request","input":{"prompt":"New request"},
            "result":null,"state":"queued","awaiting_child_id":null,
            "revision":1,"created_at_ms":3,"updated_at_ms":3
        }));
    }
    let advanced = command(&service, SharedWorkCommand::Refresh).await;
    assert!(advanced.error.is_none(), "{:?}", advanced.error);
    assert_eq!(advanced.selected_job_id.as_deref(), Some("job-revised"));
    assert!(advanced.approval.is_none());
    server.stop().await;
}
#[tokio::test]
async fn settled_child_returns_to_parent_with_followup_available() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"Team","role":"contributor",
            "can_submit":true,"participation_generation":1
        })];
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
        script.job = Some(json!({
            "id":"job-a","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":null,"environment_id":"env-a",
            "title":"Build the app","input":{"prompt":"Build the app"},
            "result":null,"state":"waiting_child","awaiting_child_id":"job-child",
            "revision":1,"created_at_ms":1,"updated_at_ms":1
        }));
        script.child_job = Some(json!({
            "id":"job-child","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":"job-a","environment_id":"env-b",
            "title":"Host the app","input":{"prompt":"Host the app"},
            "result":null,"state":"running","awaiting_child_id":null,
            "revision":1,"created_at_ms":2,"updated_at_ms":2
        }));
        script.child_approval = Some(json!({
            "id":"approval-child","attempt_id":"attempt-child","status":"decided",
            "decision":"approve","expires_at_ms":now_ms()+300_000,"can_decide":false,
            "request":{"access":"shell","summary":"Run a shell check",
                "details":["Command: Get-ChildItem -LiteralPath ."],"targets":["C:/workspace"],
                "outside_workspace":false,"risks":["unclassified_shell"]}
        }));
        script.inbox_items = vec![json!({
            "id":"notification-child","job_id":"job-child","project_id":"project-a",
            "kind":"approval","title":"Host the app","created_at_ms":2,
            "read_at_ms":2,"can_act":false,"approval_id":"approval-child",
            "approval_status":"decided","approval_decision":"approve"
        })];
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    assert!(
        command(&service, SharedWorkCommand::Refresh)
            .await
            .error
            .is_none()
    );
    let child = command(
        &service,
        SharedWorkCommand::InboxOpen {
            notification_id: "notification-child".into(),
        },
    )
    .await;
    assert!(child.error.is_none(), "{:?}", child.error);
    assert_eq!(child.selected_job_id.as_deref(), Some("job-child"));
    assert_eq!(
        child
            .approval
            .as_ref()
            .and_then(|approval| approval.decision.as_deref()),
        Some("approve")
    );
    let notification = &child.inbox.as_ref().unwrap().items[0];
    assert_eq!(notification.approval_status.as_deref(), Some("decided"));
    assert_eq!(notification.approval_decision.as_deref(), Some("approve"));
    let projected = serde_json::to_value(notification).unwrap();
    assert_eq!(projected["approval_status"], "decided");
    assert_eq!(projected["approval_decision"], "approve");
    assert_eq!(
        command(&service, SharedWorkCommand::Refresh)
            .await
            .selected_job_id
            .as_deref(),
        Some("job-child")
    );

    {
        let mut script = server.script.lock().unwrap();
        let child = script.child_job.as_mut().unwrap();
        child["state"] = json!("succeeded");
        let parent = script.job.as_mut().unwrap();
        parent["state"] = json!("succeeded");
        parent["awaiting_child_id"] = Value::Null;
        parent["can_continue"] = json!(true);
    }
    let observed_child = command(&service, SharedWorkCommand::Refresh).await;
    assert!(observed_child.error.is_none(), "{:?}", observed_child.error);
    assert_eq!(
        observed_child
            .detail
            .as_ref()
            .map(|detail| detail.state.as_str()),
        Some("succeeded")
    );
    let parent = command(&service, SharedWorkCommand::Refresh).await;
    assert!(parent.error.is_none(), "{:?}", parent.error);
    assert_eq!(parent.selected_job_id.as_deref(), Some("job-a"));
    assert_eq!(
        parent.detail.as_ref().map(|detail| detail.id.as_str()),
        Some("job-a")
    );
    assert!(
        parent
            .detail
            .as_ref()
            .is_some_and(|detail| detail.can_continue)
    );
    server.stop().await;
}
#[tokio::test]
async fn lost_leave_reply_stays_pending_and_retries_same_participation_after_restart() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"Team","role":"contributor",
            "can_submit":true,"participation_generation":2
        })];
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
        script.leave_once_unavailable = true;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let first = service(&root, &server.shared, "WinA").await;
    let loaded = command(&first, SharedWorkCommand::Refresh).await;
    assert!(loaded.error.is_none(), "{:?}", loaded.error);
    let pending = command(
        &first,
        SharedWorkCommand::LeaveProject {
            project_id: "project-a".into(),
        },
    )
    .await;
    assert_eq!(
        pending.leave_pending_project_id.as_deref(),
        Some("project-a")
    );
    assert!(pending.error.is_some());
    drop(first);

    let restored = service(&root, &server.shared, "WinA").await;
    let settled = command(&restored, SharedWorkCommand::Refresh).await;
    assert!(settled.error.is_none(), "{:?}", settled.error);
    assert!(settled.leave_pending_project_id.is_none());
    assert!(settled.projects.is_empty());
    let script = server.script.lock().unwrap();
    assert_eq!(script.leave_requests.len(), 2);
    assert_eq!(script.leave_requests[0], script.leave_requests[1]);
    assert_eq!(
        script.leave_requests[0]["expected_participation_generation"],
        2
    );
    drop(script);
    server.stop().await;
}
#[tokio::test]
async fn execution_only_pc_can_leave_its_own_project() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.device_projects = Some(vec![json!({
            "id":"project-a","label":"Team","can_control":false,"can_execute":true,
            "participation_generation":7,"environment_id":"env-b",
            "preparation_state":"ready","error":null
        })]);
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinB").await;
    let loaded = command(&service, SharedWorkCommand::Refresh).await;
    assert!(loaded.error.is_none(), "{:?}", loaded.error);
    assert!(loaded.projects.is_empty());
    service.set_execution_projects_for_test(vec![crate::device_network::DeviceProject {
        id: "project-a".into(),
        label: "Team".into(),
        can_execute: true,
        participation_generation: 6,
        environment_id: Some("env-b".into()),
        preparation_state: "ready".into(),
        ..Default::default()
    }]);
    let stale = command(
        &service,
        SharedWorkCommand::LeaveProject {
            project_id: "project-a".into(),
        },
    )
    .await;
    assert!(
        stale
            .error
            .as_deref()
            .is_some_and(|error| error.contains("更新"))
    );
    assert!(server.script.lock().unwrap().leave_requests.is_empty());
    service.set_execution_projects_for_test(vec![crate::device_network::DeviceProject {
        id: "project-a".into(),
        label: "Team".into(),
        can_execute: true,
        participation_generation: 7,
        environment_id: Some("env-b".into()),
        preparation_state: "ready".into(),
        ..Default::default()
    }]);
    let left = command(
        &service,
        SharedWorkCommand::LeaveProject {
            project_id: "project-a".into(),
        },
    )
    .await;
    assert!(left.error.is_none(), "{:?}", left.error);
    assert!(left.leave_pending_project_id.is_none());
    assert_eq!(
        server.script.lock().unwrap().leave_requests[0]["expected_participation_generation"],
        7
    );
    server.stop().await;
}
#[tokio::test]
async fn stopped_latest_shared_prompt_resends_as_revised_turn_in_same_conversation() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"Team","role":"contributor",
            "can_submit":true,"participation_generation":2
        })];
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
        script.job = Some(json!({
            "id":"job-a","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":null,"environment_id":"env-a",
            "title":"Old prompt","input":{"version":2,"prompt":"Old prompt","input_refs":[]},
            "result":null,"state":"cancelled","awaiting_child_id":null,
            "revision":4,"created_at_ms":1,"updated_at_ms":2,"can_revise":true
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    assert!(
        command(&service, SharedWorkCommand::Refresh)
            .await
            .error
            .is_none()
    );
    let selected = command(
        &service,
        SharedWorkCommand::SelectConversation {
            project_id: "project-a".into(),
            conversation_id: "job-a".into(),
        },
    )
    .await;
    assert!(selected.error.is_none(), "{:?}", selected.error);
    assert!(selected.detail.as_ref().unwrap().can_revise);
    let revised = command(
        &service,
        SharedWorkCommand::Revise {
            project_id: "project-a".into(),
            conversation_id: "job-a".into(),
            job_id: "job-a".into(),
            expected_revision: 4,
            prompt: "WinB hosts the corrected Flask app".into(),
        },
    )
    .await;
    assert!(
        revised.error.is_none(),
        "{:?}; requests={:?}",
        revised.error,
        server.script.lock().unwrap().requests
    );
    assert_eq!(revised.selected_conversation_id.as_deref(), Some("job-a"));
    assert_eq!(revised.selected_job_id.as_deref(), Some("job-revised"));
    let script = server.script.lock().unwrap();
    let request = script.submit_requests.last().unwrap();
    assert_eq!(request["conversation_id"], "job-a");
    assert_eq!(request["revises_job_id"], "job-a");
    assert_eq!(request["expected_revised_revision"], 4);
    assert_eq!(request["project_participation"], 2);
    drop(script);
    server.stop().await;
}
#[tokio::test]
async fn old_shared_job_cannot_continue_when_conversation_has_a_newer_turn() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"Team","role":"contributor",
            "can_submit":true,"participation_generation":2
        })];
        script.conversation_latest_job_id = Some("job-b".into());
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
        script.job = Some(json!({
            "id":"job-a","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":null,"environment_id":"env-a",
            "title":"Earlier prompt","input":{"version":2,"prompt":"Earlier prompt","input_refs":[]},
            "result":null,"state":"completed","awaiting_child_id":null,
            "revision":4,"created_at_ms":1,"updated_at_ms":2,"can_continue":true
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    let loaded = command(&service, SharedWorkCommand::Refresh).await;
    assert!(loaded.error.is_none(), "{:?}", loaded.error);
    let selected = command(
        &service,
        SharedWorkCommand::Detail {
            project_id: "project-a".into(),
            job_id: "job-a".into(),
        },
    )
    .await;
    assert!(selected.error.is_none(), "{:?}", selected.error);
    assert!(selected.detail.as_ref().unwrap().can_continue);
    let blocked = command(
        &service,
        SharedWorkCommand::Continue {
            project_id: "project-a".into(),
            job_id: "job-a".into(),
            expected_revision: 4,
            prompt: "Follow up".into(),
            start_before_ms: None,
        },
    )
    .await;
    assert!(
        blocked
            .error
            .as_deref()
            .is_some_and(|error| error.contains("最新"))
    );
    assert!(server.script.lock().unwrap().submit_requests.is_empty());
    server.stop().await;
}
#[tokio::test]
async fn shared_conversation_rename_and_delete_use_the_listed_revision() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"Team","role":"contributor",
            "can_submit":true,"participation_generation":1
        })];
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
        script.job = Some(json!({
            "id":"job-a","conversation_id":"job-a","project_id":"project-a",
            "root_id":"job-a","parent_id":null,"environment_id":"env-a",
            "title":"Initial","input":{"version":2,"prompt":"Initial","input_refs":[]},
            "result":null,"state":"succeeded","awaiting_child_id":null,
            "revision":1,"created_at_ms":1,"updated_at_ms":1
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    assert!(
        command(&service, SharedWorkCommand::Refresh)
            .await
            .error
            .is_none()
    );
    let renamed = command(
        &service,
        SharedWorkCommand::RenameConversation {
            project_id: "project-a".into(),
            conversation_id: "job-a".into(),
            title: "Renamed".into(),
        },
    )
    .await;
    assert!(renamed.error.is_none(), "{:?}", renamed.error);
    assert_eq!(renamed.conversations[0].title, "Renamed");
    let deleted = command(
        &service,
        SharedWorkCommand::DeleteConversation {
            project_id: "project-a".into(),
            conversation_id: "job-a".into(),
        },
    )
    .await;
    assert!(deleted.error.is_none(), "{:?}", deleted.error);
    assert!(deleted.conversations.is_empty());
    let script = server.script.lock().unwrap();
    assert_eq!(script.rename_requests[0]["expected_revision"], 1);
    assert_eq!(script.delete_requests[0]["expected_revision"], 2);
    drop(script);
    server.stop().await;
}
#[tokio::test]
async fn execution_only_pc_reaches_device_project_authority_for_folder_binding() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinB").await;
    assert!(
        command(&service, SharedWorkCommand::Refresh)
            .await
            .error
            .is_none()
    );
    assert!(service.shared_work_projection().projects.is_empty());
    let result = command(
        &service,
        SharedWorkCommand::BindProjectFolder {
            project_id: "project-a".into(),
            environment_id: "env-b".into(),
            directory: root.join("existing-work"),
            access_mode: crate::config::AccessMode::Default,
            expected_directory: None,
        },
    )
    .await;
    assert_eq!(
        result.error.as_deref(),
        Some("このPCの実行対象が変わりました。最新のプロジェクト設定を確認してください。")
    );
    assert!(
        server
            .script
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(_, path)| path == "/v1/shared/device-projects")
    );
    server.stop().await;
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
async fn expired_session_keeps_last_confirmed_projects_read_only_during_hub_outage() {
    let server = Server::start().await;
    {
        let mut script = server.script.lock().unwrap();
        script.projects = vec![json!({
            "id":"project-a","label":"Team","role":"contributor",
            "can_submit":true,"participation_generation":1
        })];
        script.status = Some(json!({
            "project_id":"project-a","jobs":[],"environments":[],
            "next_before":null,"next_environment_before":null
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_owned()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    let initial = command(&service, SharedWorkCommand::Refresh).await;
    assert!(!initial.projects_stale);
    assert_eq!(initial.projects.len(), 1);
    expire(&service);
    server.script.lock().unwrap().unavailable = true;
    let stale = command(&service, SharedWorkCommand::Refresh).await;
    assert!(stale.error.is_some());
    assert!(stale.principal.is_none());
    assert!(stale.projects_stale);
    assert_eq!(stale.projects[0].label, "Team");
    server.script.lock().unwrap().unavailable = false;
    let restored = command(&service, SharedWorkCommand::Refresh).await;
    assert!(restored.error.is_none());
    assert!(restored.principal.is_some());
    assert!(!restored.projects_stale);
    server.stop().await;
}

#[tokio::test]
async fn ordinary_agent_uses_authorized_pc_and_keeps_local_history_private() {
    let server = Server::start().await;
    let input_bytes = b"input\n".to_vec();
    let input_sha = format!("{:x}", sha2::Sha256::digest(&input_bytes));
    {
        let mut script = server.script.lock().unwrap();
        script.projects =
            vec![json!({"id":"project-a","label":"Team","role":"contributor","can_submit":true})];
        script.environments = vec![json!({"id":"env-b","label":"WinB","device_id":"device-b",
            "device_label":"WinB","enabled":true,"can_submit":true,"capacity":1,"occupied":0,
            "capabilities":[{"name":"Flask","source":"declared","observed_at_ms":null}]})];
        script.job = Some(
            json!({"id":"job-a","conversation_id":"job-a","origin_session_ref":"local-session-a",
            "origin_turn_ref":"turn-a","origin_turn_epoch":1,
            "project_id":"project-a","root_id":"job-a","parent_id":null,"environment_id":"env-b",
            "title":"TODO app","input":{"version":2,"prompt":"Build a TODO app","input_refs":["asset-a"]},
            "result":{"version":1,"text":"Done"},"state":"succeeded","awaiting_child_id":null,
            "retained_services":[],"revision":2,"created_at_ms":1,"updated_at_ms":2}),
        );
        script.asset = Some(
            json!({"id":"asset-a","project_id":"project-a","job_id":null,
            "kind":"input","name":"input.csv","sha256":input_sha,"byte_length":input_bytes.len(),
            "created_at_ms":1,"version":1,"base_sha256":null,"purged_at_ms":null}),
        );
        let output_bytes = b"generated output\n";
        let output = json!({"id":"output-a","project_id":"project-a","job_id":"job-a",
            "kind":"artifact","name":"result.txt","sha256":format!("{:x}",sha2::Sha256::digest(output_bytes)),
            "byte_length":output_bytes.len(),"created_at_ms":2,"version":1,
            "base_sha256":null,"purged_at_ms":null});
        script.artifacts = vec![script.asset.clone().unwrap(), output.clone()];
        script.downloaded_artifact = Some(json!({"asset":output,
            "content_base64":base64::engine::general_purpose::STANDARD.encode(output_bytes)}));
    }
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let service = service(&root, &server.shared, "WinA").await;
    let listed = service
        .agent_environments(Some("project-a"), Some("WinB"))
        .await
        .unwrap();
    assert_eq!(listed["environments"][0]["device_id"], "device-b");
    assert_eq!(
        listed["environments"][0]["capabilities"][0]["name"],
        "Flask"
    );
    let control = crate::runtime::RunControl::new();
    let uploaded = service
        .agent_upload_file(
            "asset-request-a",
            "project-a",
            "input.csv",
            input_bytes,
            || Ok(control.begin_tool_effect_commit().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(uploaded["asset_id"], "asset-a");
    let submitted = service
        .agent_submit_job(
            "tool-call-a",
            "local-session-a",
            "turn-a",
            1,
            "project-a",
            "env-b",
            "TODO app",
            "Build a TODO app",
            &["asset-a".into()],
            || Ok(control.begin_tool_effect_commit().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(submitted["job_id"], "job-a");
    assert_eq!(submitted["conversation_id"], "job-a");
    let request = server.script.lock().unwrap().submitted_job.clone().unwrap();
    assert_eq!(request["origin_session_ref"], "local-session-a");
    assert_eq!(request["environment_id"], "env-b");
    assert_eq!(
        request["input"],
        json!({"version":2,"prompt":"Build a TODO app","input_refs":["asset-a"]})
    );
    let observed = service
        .agent_job_status("project-a", "job-a")
        .await
        .unwrap();
    assert_eq!(observed["state"], "succeeded");
    assert_eq!(observed["result"]["text"], "Done");
    assert_eq!(observed["artifacts"]["items"][0]["asset_id"], "output-a");
    let output = service
        .agent_read_artifact_text("project-a", "job-a", "output-a")
        .await
        .unwrap();
    assert_eq!(output["content"], "generated output\n");
    let binary = vec![0xa5; 70 * 1024];
    let binary_hash = format!("{:x}", sha2::Sha256::digest(&binary));
    {
        let mut script = server.script.lock().unwrap();
        script.artifacts[1]["name"] = json!("result.bin");
        script.artifacts[1]["sha256"] = json!(binary_hash);
        script.artifacts[1]["byte_length"] = json!(binary.len());
        let asset = script.artifacts[1].clone();
        script.downloaded_artifact = Some(json!({"asset":asset,
            "content_base64":base64::engine::general_purpose::STANDARD.encode(&binary)}));
    }
    assert!(
        service
            .agent_read_artifact_text("project-a", "job-a", "output-a")
            .await
            .is_err()
    );
    let (asset, saved_bytes) = service
        .agent_download_artifact("project-a", "job-a", "output-a")
        .await
        .unwrap();
    assert_eq!(asset.name, "result.bin");
    assert_eq!(saved_bytes, binary);
    server
        .script
        .lock()
        .unwrap()
        .downloaded_artifact
        .as_mut()
        .unwrap()["asset"]["version"] = json!(2);
    assert!(
        service
            .agent_download_artifact("project-a", "job-a", "output-a")
            .await
            .is_err()
    );
    server
        .script
        .lock()
        .unwrap()
        .downloaded_artifact
        .as_mut()
        .unwrap()["asset"]["version"] = json!(1);
    assert!(
        service
            .agent_read_artifact_text("project-a", "job-a", "asset-a")
            .await
            .is_err()
    );
    assert!(
        service
            .agent_read_artifact_text("other-project", "job-a", "output-a")
            .await
            .is_err()
    );
    server
        .script
        .lock()
        .unwrap()
        .downloaded_artifact
        .as_mut()
        .unwrap()["content_base64"] =
        json!(base64::engine::general_purpose::STANDARD.encode(b"tampered"));
    assert!(
        service
            .agent_download_artifact("project-a", "job-a", "output-a")
            .await
            .is_err()
    );
    {
        server.script.lock().unwrap().environments[0]["can_submit"] = json!(false);
    }
    let before = server
        .script
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(method, path)| method == "POST" && path == "/v1/shared/jobs")
        .count();
    assert!(
        service
            .agent_submit_job(
                "tool-call-b",
                "local-session-a",
                "turn-a",
                1,
                "project-a",
                "env-b",
                "Second job",
                "Do it",
                &[],
                || Ok(control.begin_tool_effect_commit().unwrap())
            )
            .await
            .is_err()
    );
    let after = server
        .script
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(method, path)| method == "POST" && path == "/v1/shared/jobs")
        .count();
    assert_eq!(after, before);
    {
        let mut script = server.script.lock().unwrap();
        script.environments[0]["can_submit"] = json!(true);
        script.environments[0]["device_id"] = json!("WinA");
    }
    assert!(
        service
            .agent_submit_job(
                "tool-call-c",
                "local-session-a",
                "turn-a",
                1,
                "project-a",
                "env-b",
                "Self job",
                "Do it",
                &[],
                || Ok(control.begin_tool_effect_commit().unwrap())
            )
            .await
            .is_err()
    );
    let after_self = server
        .script
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(method, path)| method == "POST" && path == "/v1/shared/jobs")
        .count();
    assert_eq!(after_self, before);
    drop(service);
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
