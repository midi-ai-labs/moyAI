use super::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    _temp: tempfile::TempDir,
    service: DeviceNetworkService,
    old: SharedHubConfig,
    candidate: SharedHubConfig,
    reply: Arc<Mutex<serde_json::Value>>,
    paths: Arc<Mutex<Vec<String>>>,
    cancel: CancellationToken,
    server: tokio::task::JoinHandle<()>,
    config_file: Utf8PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let (temp, service) = super::super::lifecycle_tests::fixture().await;
        let identity = service.inner.identity.load_or_create().unwrap();
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let ca = params.self_signed(&ca_key).unwrap().pem();
        let issuer = rcgen::Issuer::new(params, ca_key);
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
        let fingerprint = format!("{:x}", Sha256::digest(leaf.der().as_ref()));
        let server_key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec![
            "127.0.0.1".into(),
            crate::mcp_publish::tls::HUB_TLS_ROLE_NAME.into(),
        ])
        .unwrap();
        let server_leaf = params.signed_by(&server_key, &issuer).unwrap();
        let acceptor = crate::mcp_publish::tls::load_mtls_acceptor(
            &server_leaf.pem(),
            &server_key.serialize_pem(),
            &ca,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let candidate = SharedHubConfig {
            hub_url: format!("https://{}", listener.local_addr().unwrap()),
            ca_certificate_pem: ca.clone(),
        };
        let old = SharedHubConfig {
            hub_url: "https://127.0.0.1:1".into(),
            ca_certificate_pem: ca,
        };
        let expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 86_400_000;
        {
            let mut state = service.inner.state.lock().unwrap();
            let mut settings = state.settings.clone();
            settings.hub_id = Some("hub".into());
            settings.device_id = Some("device".into());
            settings.label = "existing device".into();
            settings.certificate_pem = Some(leaf.pem());
            settings.certificate_sha256 = Some(fingerprint.clone());
            settings.expires_at_ms = Some(expiry.to_string());
            state.settings = service.inner.settings.save(&settings).unwrap();
            state.client = Some(
                DeviceClient::new(&old, Some((&identity, &leaf.pem())), "device".into()).unwrap(),
            );
            state.identity = Some(identity);
            state.shared = old.clone();
            state.status = "active";
        }
        let config_file = service.inner.directory.join("fixture-config.toml");
        std::fs::write(&config_file, toml::to_string(&old).unwrap()).unwrap();
        let reply = Arc::new(Mutex::new(
            serde_json::json!({"hub_id":"hub","device_id":"device","label":"existing device","groups":[],"revision":"2","certificate_sha256":fingerprint,"expires_at_ms":expiry,"admission":"allowed"}),
        ));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let cancel = CancellationToken::new();
        let (server_reply, server_paths, shutdown) = (reply.clone(), paths.clone(), cancel.clone());
        let server = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! { _ = shutdown.cancelled() => break, value = listener.accept() => value };
                let Ok((tcp, _)) = accepted else { break };
                let request = async {
                    let mut stream = acceptor.accept(tcp).await.unwrap();
                    assert_eq!(
                        format!(
                            "{:x}",
                            Sha256::digest(
                                stream.get_ref().1.peer_certificates().unwrap()[0].as_ref()
                            )
                        ),
                        fingerprint
                    );
                    let mut bytes = Vec::new();
                    while !bytes.ends_with(b"\r\n\r\n") {
                        bytes.push(stream.read_u8().await.unwrap());
                        assert!(bytes.len() < 8192);
                    }
                    let text = String::from_utf8(bytes).unwrap();
                    server_paths
                        .lock()
                        .unwrap()
                        .push(text.lines().next().unwrap().to_owned());
                    let body = serde_json::to_vec(&*server_reply.lock().unwrap()).unwrap();
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                    stream.write_all(&body).await.unwrap();
                    stream.shutdown().await.unwrap();
                };
                tokio::select! { _ = shutdown.cancelled() => break, _ = request => {} }
            }
        });
        Self {
            _temp: temp,
            service,
            old,
            candidate,
            reply,
            paths,
            cancel,
            server,
            config_file,
        }
    }
    fn unchanged(&self, disk: &[u8], key: &[u8], registration: &[u8]) {
        assert_eq!(self.service.inner.state.lock().unwrap().shared, self.old);
        assert_eq!(std::fs::read(&self.config_file).unwrap(), disk);
        assert_eq!(
            std::fs::read(self.service.inner.directory.join("identity.json")).unwrap(),
            key
        );
        assert_eq!(
            std::fs::read(self.service.inner.directory.join("device.json")).unwrap(),
            registration
        );
    }
    fn originals(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (
            std::fs::read(&self.config_file).unwrap(),
            std::fs::read(self.service.inner.directory.join("identity.json")).unwrap(),
            std::fs::read(self.service.inner.directory.join("device.json")).unwrap(),
        )
    }
    async fn close(self) {
        self.cancel.cancel();
        self.server.await.unwrap();
    }
}

#[tokio::test]
async fn same_hub_endpoint_move_reuses_mtls_identity_and_exact_existing_ca() {
    let f = Fixture::new().await;
    let (_, key, registration) = f.originals();
    let before = f.service.projection_now();
    let mut candidate = f.candidate.clone();
    candidate.ca_certificate_pem = candidate.ca_certificate_pem.replace('\n', "\r\n");
    let prepared = f
        .service
        .prepare_configuration(candidate, &before.revision, &before.generation)
        .await
        .unwrap();
    assert!(prepared.endpoint_changed());
    assert_eq!(
        f.service.inner.state.lock().unwrap().shared,
        f.old,
        "verification does not mutate the live route"
    );
    f.service
        .commit_configuration(prepared, |saved| {
            assert_eq!(saved.ca_certificate_pem, f.old.ca_certificate_pem);
            std::fs::write(&f.config_file, toml::to_string(saved).unwrap()).unwrap();
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        f.service
            .inner
            .state
            .lock()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .endpoint(),
        f.candidate.hub_url
    );
    assert_eq!(
        std::fs::read(f.service.inner.directory.join("identity.json")).unwrap(),
        key
    );
    assert_eq!(
        std::fs::read(f.service.inner.directory.join("device.json")).unwrap(),
        registration
    );
    assert_eq!(
        *f.paths.lock().unwrap(),
        vec!["GET /v1/network/self HTTP/1.1"]
    );
    f.close().await;
}

#[tokio::test]
async fn endpoint_verification_rejects_changed_trust_and_wrong_server_registration_without_commit()
{
    let f = Fixture::new().await;
    let (disk, key, registration) = f.originals();
    let before = f.service.projection_now();
    let mut wrong_ca = f.candidate.clone();
    let keypair = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::default();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    wrong_ca.ca_certificate_pem = ca.self_signed(&keypair).unwrap().pem();
    assert_eq!(
        f.service
            .prepare_configuration(wrong_ca, &before.revision, &before.generation)
            .await
            .err(),
        Some(DeviceError::DifferentHub)
    );
    assert!(f.paths.lock().unwrap().is_empty());
    for (field, value) in [
        ("hub_id", serde_json::json!("different")),
        ("device_id", serde_json::json!("different")),
        ("certificate_sha256", serde_json::json!("b".repeat(64))),
        ("expires_at_ms", serde_json::json!(1)),
        ("revision", serde_json::json!("01")),
    ] {
        let old = f.reply.lock().unwrap()[field].clone();
        f.reply.lock().unwrap()[field] = value;
        assert_eq!(
            f.service
                .prepare_configuration(f.candidate.clone(), &before.revision, &before.generation)
                .await
                .err(),
            Some(DeviceError::DifferentHub),
            "{field}"
        );
        f.reply.lock().unwrap()[field] = old;
        f.unchanged(&disk, &key, &registration);
    }
    let unavailable = SharedHubConfig {
        hub_url: "https://127.0.0.1:1".into(),
        ..f.candidate.clone()
    };
    // Use an unreachable address different from the existing address.
    let unavailable = SharedHubConfig {
        hub_url: unavailable.hub_url.replace(":1", ":2"),
        ..unavailable
    };
    assert!(
        f.service
            .prepare_configuration(unavailable, &before.revision, &before.generation)
            .await
            .is_err()
    );
    f.unchanged(&disk, &key, &registration);
    f.close().await;
}

#[tokio::test]
async fn verified_endpoint_does_not_commit_after_target_change_or_storage_failure() {
    let f = Fixture::new().await;
    let (disk, key, registration) = f.originals();
    let before = f.service.projection_now();
    let prepared = f
        .service
        .prepare_configuration(f.candidate.clone(), &before.revision, &before.generation)
        .await
        .unwrap();
    f.service.inner.state.lock().unwrap().generation += 1;
    assert_eq!(
        f.service
            .commit_configuration(prepared, |_| panic!("stale commit"))
            .await
            .err(),
        Some(DeviceError::ConnectionChanged)
    );
    f.unchanged(&disk, &key, &registration);
    let before = f.service.projection_now();
    let prepared = f
        .service
        .prepare_configuration(f.candidate.clone(), &before.revision, &before.generation)
        .await
        .unwrap();
    assert_eq!(
        f.service
            .commit_configuration(prepared, |_| Err(DeviceError::Storage))
            .await
            .err(),
        Some(DeviceError::Storage)
    );
    f.unchanged(&disk, &key, &registration);
    f.close().await;
}
