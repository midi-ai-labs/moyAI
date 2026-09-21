use super::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct JoinFixture {
    shared: SharedHubConfig,
    script: Arc<Mutex<Script>>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
struct Script {
    status: &'static str,
    requests: Vec<String>,
    proofs: usize,
    lose_first_request: bool,
    certificate: serde_json::Value,
}
impl JoinFixture {
    async fn start(identity: &DeviceIdentity) -> Self {
        Self::start_named(identity, "hub-test", "device-local").await
    }
    async fn start_named(
        identity: &DeviceIdentity,
        hub_id: &'static str,
        device_id: &'static str,
    ) -> Self {
        use tokio_rustls::rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
        };
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::default();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let ca = ca_params.self_signed(&ca_key).unwrap().pem();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let server_key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec![
            "127.0.0.1".into(),
            crate::mcp_publish::tls::HUB_TLS_ROLE_NAME.into(),
        ])
        .unwrap();
        let server = params.signed_by(&server_key, &issuer).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(server.pem().as_bytes()).unwrap()],
                    PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes()).unwrap(),
                )
                .unwrap(),
        ));
        let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        ];
        let leaf = params
            .signed_by(
                &rcgen::KeyPair::from_pem(identity.private_key_pem()).unwrap(),
                &issuer,
            )
            .unwrap();
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 29 * 86400 * 1000;
        let certificate = serde_json::json!({"hub_id":hub_id,"device_id":device_id,"label":"登録名-日本語","certificate_pem":leaf.pem(),"ca_certificate_pem":ca,"certificate_sha256":format!("{:x}",Sha256::digest(leaf.der().as_ref())),"expires_at_ms":expires});
        let script = Arc::new(Mutex::new(Script {
            status: "pending",
            requests: vec![],
            proofs: 0,
            lose_first_request: false,
            certificate,
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let shared = SharedHubConfig {
            hub_url: format!("https://{}", listener.local_addr().unwrap()),
            ca_certificate_pem: ca,
        };
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_script = script.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! { _=run_cancel.cancelled()=>break, value=listener.accept()=>value };
                let Ok((tcp, _)) = accepted else { break };
                let acceptor = acceptor.clone();
                let script = run_script.clone();
                let result = async {
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
                        .filter_map(|line| line.split_once(':'))
                        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    assert!(length < 32768);
                    let mut bytes = vec![0; length];
                    stream.read_exact(&mut bytes).await.ok()?;
                    let body: serde_json::Value =
                        serde_json::from_slice(&bytes).unwrap_or_default();
                    let path = headers.split_whitespace().nth(1).unwrap();
                    let reply = {
                        let mut script = script.lock().unwrap();
                        match path {
                            "/v1/network/join/request" => {
                                let csr = body["csr_pem"].as_str().unwrap().to_owned();
                                assert!(csr.contains("CERTIFICATE REQUEST"));
                                assert!(!csr.contains("PRIVATE KEY"));
                                assert!(!body["label"].as_str().unwrap().is_empty());
                                script.requests.push(csr);
                                if script.lose_first_request {
                                    script.lose_first_request = false;
                                    return None;
                                }
                                serde_json::json!({"hub_id":hub_id,"request_id":"request-local","challenge":"challenge-local","expires_at_ms":expires})
                            }
                            "/v1/network/join/status" => {
                                assert_eq!(body["request_id"], "request-local");
                                assert!(
                                    body["proof_csr_pem"]
                                        .as_str()
                                        .unwrap()
                                        .contains("CERTIFICATE REQUEST")
                                );
                                script.proofs += 1;
                                serde_json::json!({"hub_id":hub_id,"request_id":"request-local","status":script.status,"certificate":if matches!(script.status,"approved"|"stopped"){script.certificate.clone()}else{serde_json::Value::Null}})
                            }
                            "/v1/network/presence" => serde_json::json!({}),
                            "/v1/network/history/requests" => serde_json::json!({"requests":[]}),
                            "/v1/network/self" => {
                                serde_json::json!({"hub_id":hub_id,"device_id":device_id,"label":"登録名-日本語","groups":[],"revision":"1","certificate_sha256":script.certificate["certificate_sha256"],"expires_at_ms":expires,"admission":if script.status=="stopped"{"stopped"}else{"allowed"}})
                            }
                            "/v1/network/directory" => {
                                serde_json::json!({"hub_id":hub_id,"revision":"1","peers":[]})
                            }
                            "/v1/network/model-session" => {
                                serde_json::json!({"id":"model-client","client_token":"runtime-secret-01234567890123456789012345","hub_id":hub_id,"revision":"1","heartbeat_interval_ms":10000,"identity_scope":"device_session","default_selection":{"allowed_model_ids":["model"],"preferred_model_id":"model","required_capabilities":["tools"],"wait_policy":"allow_selected_fallback","affinity_turns":3}})
                            }
                            "/v1/catalog" => {
                                serde_json::json!({"hub_id":hub_id,"software_version":"0.1.0","revision":"1","models":[{"id":"model","label":"Model","capabilities":["tools"]}],"changes":[]})
                            }
                            "/v1/clients/review" => {
                                serde_json::json!({"id":body["id"],"context":body["context"],"reviewed_revision":body["reviewed_revision"]})
                            }
                            "/v1/clients/disconnect" => {
                                serde_json::json!({"id":body["id"],"disconnected":true})
                            }
                            _ => panic!("unexpected request {path}"),
                        }
                    };
                    let body = serde_json::to_vec(&reply).unwrap();
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.ok()?;
                    stream.write_all(&body).await.ok()?;
                    stream.shutdown().await.ok()?;
                    Some(())
                };
                let _ = tokio::select! {_=run_cancel.cancelled()=>break,value=result=>value};
            }
        });
        Self {
            shared,
            script,
            cancel,
            task,
        }
    }
    async fn stop(self) {
        self.cancel.cancel();
        self.task.await.unwrap();
    }
}

async fn configure(service: &DeviceNetworkService, shared: SharedHubConfig) {
    let projection = service.projection_now();
    service
        .configure(shared.clone(), &projection.revision, &projection.generation)
        .await
        .unwrap();
    service.inner.global_config.lock().unwrap().device_network = shared;
}

#[tokio::test]
async fn csr_cache_rejects_late_retired_key_writes_and_unbound_legacy_entries() {
    let (_temp, service) = lifecycle_tests::fixture().await;
    let old_identity = service.inner.identity.load_or_create().unwrap();
    let old_csr = service
        .stable_csr(&old_identity, Ipv4Addr::LOCALHOST)
        .unwrap();
    let before = service.projection_now();
    service
        .reset_local(&before.revision, &before.generation, |_| Ok(()))
        .await
        .unwrap();
    let current = service.inner.identity.load_or_create().unwrap();
    assert_ne!(
        old_identity.public_key_sha256().unwrap(),
        current.public_key_sha256().unwrap()
    );

    // A process that captured the retired identity may complete its write after
    // reset removed the old cache, even at the exact same network address.
    let late_old_csr = service
        .stable_csr(&old_identity, Ipv4Addr::LOCALHOST)
        .unwrap();
    let new_csr = service.stable_csr(&current, Ipv4Addr::LOCALHOST).unwrap();
    assert_ne!(new_csr, late_old_csr);
    assert_eq!(
        new_csr,
        service.stable_csr(&current, Ipv4Addr::LOCALHOST).unwrap()
    );

    // Pre-binding cache files are accepted for reading but never trusted as
    // current-key CSRs merely because the IP matches.
    let path = service.inner.directory.join("pending-csr.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"ip":"127.0.0.1","csr_pem":old_csr})).unwrap(),
    )
    .unwrap();
    let upgraded = service.stable_csr(&current, Ipv4Addr::LOCALHOST).unwrap();
    assert_ne!(upgraded, old_csr);
    let cache: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(cache["key_sha256"], current.public_key_sha256().unwrap());
    assert_eq!(
        upgraded,
        service.stable_csr(&current, Ipv4Addr::LOCALHOST).unwrap()
    );
    service.shutdown().await;
}

#[tokio::test]
async fn offline_reset_allows_first_request_and_approval_on_another_hub_without_restart() {
    let (_temp, service) = lifecycle_tests::fixture().await;
    let old_identity = service.inner.identity.load_or_create().unwrap();
    let old_hub = JoinFixture::start(&old_identity).await;
    configure(&service, old_hub.shared.clone()).await;
    old_hub.script.lock().unwrap().status = "approved";
    let before = service.projection_now();
    let active = service
        .request_join(&before.revision, &before.generation)
        .await
        .unwrap();
    assert_eq!(active.enrollment, "active");
    let old_poll_cancel = service.inner.state.lock().unwrap().cancellation.clone();
    old_hub.stop().await;
    let reset = service
        .reset_local(&active.revision, &active.generation, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(reset.enrollment, "unconfigured");
    assert!(old_poll_cancel.is_cancelled());
    let new_identity = service.inner.identity.load_or_create().unwrap();
    assert_ne!(
        old_identity.public_key_sha256().unwrap(),
        new_identity.public_key_sha256().unwrap()
    );
    let new_hub =
        JoinFixture::start_named(&new_identity, "replacement-hub", "replacement-device").await;
    configure(&service, new_hub.shared.clone()).await;
    let before = service.projection_now();
    let pending = service
        .request_join(&before.revision, &before.generation)
        .await
        .unwrap();
    assert_eq!(
        pending.enrollment, "pending",
        "the first request after reset must not inherit the cancelled old lifecycle"
    );
    assert!(pending.error.is_none());
    assert_eq!(new_hub.script.lock().unwrap().requests.len(), 1);
    assert_eq!(new_hub.script.lock().unwrap().proofs, 1);
    new_hub.script.lock().unwrap().status = "approved";
    let active = service.refresh().await.unwrap();
    assert_eq!(active.enrollment, "active");
    assert_eq!(active.device_id.as_deref(), Some("replacement-device"));
    assert!(service.client().is_ok());
    service.shutdown().await;
    new_hub.stop().await;
}

#[tokio::test]
async fn automatic_join_reuses_a_lost_request_and_reopens_pending_before_approval_without_granting_reception()
 {
    let (_temp, service) = lifecycle_tests::fixture().await;
    let identity = service.inner.identity.load_or_create().unwrap();
    let hub = JoinFixture::start(&identity).await;
    configure(&service, hub.shared.clone()).await;
    hub.script.lock().unwrap().lose_first_request = true;
    let before = service.projection_now();
    let lost = service
        .request_join(&before.revision, &before.generation)
        .await
        .unwrap();
    assert_eq!(lost.enrollment, "error");
    let pending = service.refresh().await.unwrap();
    assert_eq!(pending.enrollment, "pending");
    assert_eq!(pending.local_ipv4.as_deref(), Some("127.0.0.1"));
    assert!(pending.device_id.is_none());
    assert!(!pending.receiver.enabled);
    let requests = hub.script.lock().unwrap().requests.clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    let projection = serde_json::to_string(&pending).unwrap();
    assert!(!projection.contains("challenge-local"));
    assert!(!projection.contains("CERTIFICATE REQUEST"));
    let key = std::fs::read(service.inner.directory.join("identity.json")).unwrap();
    service.shutdown().await;
    let mut config = service.inner.global_config.lock().unwrap().clone();
    config.model.model = "existing-direct-model".into();
    let reopened = DeviceNetworkService::for_workspace(
        service.inner.directory.clone(),
        service
            .inner
            .directory
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("workspace"),
        service.inner.store.clone(),
        config.clone(),
    )
    .await
    .unwrap();
    assert_eq!(reopened.projection_now().enrollment, "pending");
    reopened.resume().await.unwrap();
    assert_eq!(hub.script.lock().unwrap().requests.len(), 2);
    let connection = crate::hub::HubConnection::new(crate::hub::HubSettingsStore::new(
        service.inner.directory.join("hub-test.json"),
    ));
    reopened.attach_hub_connection(connection.clone());
    connection.attach_device_network(reopened.downgrade());
    assert_eq!(
        connection.projection_now().main_mode,
        crate::hub::HubRouteMode::Hub
    );
    assert_eq!(
        connection
            .begin_turn(
                crate::hub::HubReviewContext::Main,
                tokio_util::sync::CancellationToken::new()
            )
            .unwrap_err(),
        crate::hub::HubError::Unavailable
    );
    assert!(
        reopened
            .receiver_model_route(tokio_util::sync::CancellationToken::new())
            .await
            .is_err(),
        "a pending Hub connection must not use the legacy receiver's Direct mode"
    );
    hub.script.lock().unwrap().status = "approved";
    let approved = reopened.refresh().await.unwrap();
    assert_eq!(approved.enrollment, "active");
    assert_eq!(approved.display_name, "登録名-日本語");
    assert!(!approved.receiver.enabled);
    assert!(!approved.receiver.confirmed);
    assert!(approved.peers.is_empty());
    assert_eq!(
        std::fs::read(service.inner.directory.join("identity.json")).unwrap(),
        key
    );
    assert_eq!(
        reopened.inner.global_config.lock().unwrap().model.model,
        config.model.model
    );
    let models = connection.projection_now();
    assert_eq!(models.status, crate::hub::HubConnectionStatus::Connected);
    assert_eq!(models.main_mode, crate::hub::HubRouteMode::Hub);
    assert_eq!(models.side_chat_mode, crate::hub::HubRouteMode::Hub);
    assert!(models.main_uses_default && models.side_chat_uses_default);
    assert_eq!(
        models
            .main_review
            .as_ref()
            .unwrap()
            .selection
            .preferred_model_id,
        "model"
    );
    assert!(!models.can_change_main_mode && !models.can_change_side_chat_mode);
    assert_eq!(
        models
            .recommended_main_selection
            .unwrap()
            .preferred_model_id,
        "model"
    );
    connection.shutdown().await;
    reopened.shutdown().await;
    hub.stop().await;
}

#[tokio::test]
async fn managed_model_command_receipts_match_polling_before_and_after_enrollment() {
    use crate::hub::{HubConnection, HubReviewContext, HubRouteMode, HubSettingsStore};
    let (_temp, service) = lifecycle_tests::fixture().await;
    let identity = service.inner.identity.load_or_create().unwrap();
    let hub = JoinFixture::start(&identity).await;
    configure(&service, hub.shared.clone()).await;
    let connection = HubConnection::new(HubSettingsStore::new(
        service.inner.directory.join("hub-receipt-test.json"),
    ));
    service.attach_hub_connection(connection.clone());
    connection.attach_device_network(service.downgrade());
    let before = service.projection_now();
    let pending = service
        .request_join(&before.revision, &before.generation)
        .await
        .unwrap();
    assert_eq!(pending.enrollment, "pending");
    let assert_receipt = |receipt: &crate::hub::HubConnectionProjection| {
        assert_eq!(receipt.endpoint, hub.shared.hub_url);
        assert_eq!(receipt.main_mode, HubRouteMode::Hub);
        assert_eq!(receipt.side_chat_mode, HubRouteMode::Hub);
        assert!(!receipt.can_change_main_mode && !receipt.can_change_side_chat_mode);
        assert_eq!(
            serde_json::to_value(receipt).unwrap(),
            serde_json::to_value(connection.projection_now()).unwrap(),
            "The command receipt and unchanged poll must expose identical effective settings"
        );
    };
    let pending_disconnect = connection
        .disconnect(connection.projection_now().connection_generation)
        .await
        .unwrap();
    assert_receipt(&pending_disconnect);
    hub.script.lock().unwrap().status = "approved";
    assert_eq!(service.refresh().await.unwrap().enrollment, "active");
    let mut current = connection
        .refresh(connection.projection_now().connection_generation)
        .await
        .unwrap();
    assert_receipt(&current);
    assert!(current.main_uses_default && current.side_chat_uses_default);
    // The GUI refreshes, saves Main explicitly, then saves Side from that exact receipt.
    // Every save uses its returned CAS revision/generation; no independent poll repairs it.
    for context in [HubReviewContext::Main, HubReviewContext::SideChat] {
        let previous_revision = current.settings_revision.clone();
        let previous_generation = current.connection_generation.clone();
        current = connection
            .save_review_with_source(
                context,
                current.recommended_main_selection.clone().unwrap(),
                current.hub_id.clone().unwrap(),
                current.catalog.as_ref().unwrap().revision,
                previous_revision.clone(),
                previous_generation.clone(),
                false,
            )
            .await
            .unwrap();
        assert_receipt(&current);
        assert_ne!(current.settings_revision, previous_revision);
        assert_eq!(current.connection_generation, previous_generation);
        assert!(!current.main_uses_default);
        assert_eq!(
            current.side_chat_uses_default,
            context == HubReviewContext::Main
        );
    }
    let unchanged_mode = connection
        .set_route_mode(
            HubReviewContext::Main,
            HubRouteMode::Hub,
            current.settings_revision,
            current.connection_generation,
        )
        .await
        .unwrap();
    assert_receipt(&unchanged_mode);
    let reset = connection.reset_local().await.unwrap();
    assert_receipt(&reset);
    assert!(reset.main_review.is_none() && reset.side_chat_review.is_none());
    connection.shutdown().await;
    service.shutdown().await;
    hub.stop().await;
}

#[tokio::test]
async fn stopped_approval_adopts_its_identity_and_reallow_resumes_without_a_new_request() {
    let (_temp, service) = lifecycle_tests::fixture().await;
    let identity = service.inner.identity.load_or_create().unwrap();
    let hub = JoinFixture::start(&identity).await;
    configure(&service, hub.shared.clone()).await;
    hub.script.lock().unwrap().status = "stopped";
    let before = service.projection_now();
    let stopped = service
        .request_join(&before.revision, &before.generation)
        .await
        .unwrap();
    assert_eq!(stopped.enrollment, "stopped");
    assert!(stopped.device_id.is_some());
    assert!(!stopped.receiver.enabled);
    assert!(!stopped.receiver.confirmed);
    assert!(service.client().is_ok());
    hub.script.lock().unwrap().status = "approved";
    assert_eq!(service.refresh().await.unwrap().enrollment, "active");
    assert_eq!(hub.script.lock().unwrap().requests.len(), 1);
    service.shutdown().await;
    hub.stop().await;
}

#[tokio::test]
async fn revoked_pending_request_is_not_resubmitted_on_restart() {
    let (_temp, service) = lifecycle_tests::fixture().await;
    let identity = service.inner.identity.load_or_create().unwrap();
    let hub = JoinFixture::start(&identity).await;
    configure(&service, hub.shared.clone()).await;
    hub.script.lock().unwrap().status = "revoked";
    let before = service.projection_now();
    service
        .request_join(&before.revision, &before.generation)
        .await
        .unwrap();
    assert_eq!(service.projection_now().enrollment, "revoked");
    service.shutdown().await;
    let config = service.inner.global_config.lock().unwrap().clone();
    let reopened = DeviceNetworkService::for_workspace(
        service.inner.directory.clone(),
        service
            .inner
            .directory
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("workspace"),
        service.inner.store.clone(),
        config,
    )
    .await
    .unwrap();
    assert_eq!(reopened.resume().await.unwrap().enrollment, "revoked");
    assert_eq!(hub.script.lock().unwrap().requests.len(), 1);
    reopened.shutdown().await;
    hub.stop().await;
}
