use super::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn certificate_renewal_after_restart_updates_existing_http_owners_before_any_listener() {
    let (_temp, service) = super::lifecycle_tests::fixture().await;
    let identity = service.inner.identity.load_or_create().unwrap();
    let key_before = std::fs::read(service.inner.directory.join("identity.json")).unwrap();
    let device_key = rcgen::KeyPair::from_pem(identity.private_key_pem()).unwrap();
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::default();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    let ca = params.self_signed(&ca_key).unwrap().pem();
    let issuer = rcgen::Issuer::new(params, ca_key);
    let leaf = |ip: &str| {
        let mut params = rcgen::CertificateParams::new(vec![ip.into()]).unwrap();
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        ];
        let certificate = params.signed_by(&device_key, &issuer).unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(certificate.der().as_ref()));
        (certificate.pem(), fingerprint)
    };
    let (old_certificate, old_fingerprint) = leaf("127.0.0.2");
    let (new_certificate, new_fingerprint) = leaf("127.0.0.1");
    let server_key = rcgen::KeyPair::generate().unwrap();
    let params = rcgen::CertificateParams::new(vec![
        "127.0.0.1".into(),
        crate::mcp_publish::tls::HUB_TLS_ROLE_NAME.into(),
    ])
    .unwrap();
    let server_certificate = params.signed_by(&server_key, &issuer).unwrap();
    let acceptor = crate::mcp_publish::tls::load_mtls_acceptor(
        &server_certificate.pem(),
        &server_key.serialize_pem(),
        &ca,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let shared = SharedHubConfig {
        hub_url: format!("https://{}", listener.local_addr().unwrap()),
        ca_certificate_pem: ca.clone(),
    };
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 30 * 86400 * 1000;
    let client = DeviceClient::new(
        &shared,
        Some((&identity, &old_certificate)),
        "device-b".into(),
    )
    .unwrap();
    let existing_owner = client.clone();
    {
        let mut state = service.inner.state.lock().unwrap();
        let mut settings = state.settings.clone();
        settings.hub_id = Some("hub".into());
        settings.device_id = Some("device-b".into());
        settings.label = "Win19".into();
        settings.certificate_pem = Some(old_certificate);
        settings.certificate_sha256 = Some(old_fingerprint.clone());
        settings.expires_at_ms = Some(expiry.to_string());
        state.settings = service.inner.settings.save(&settings).unwrap();
        state.shared = shared;
        state.identity = Some(identity);
        state.client = Some(client);
        state.status = "active";
    }
    let expected_fingerprint = new_fingerprint.clone();
    let server = tokio::spawn(async move {
        for (index, expected) in [old_fingerprint, new_fingerprint.clone()]
            .into_iter()
            .enumerate()
        {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(tcp).await.unwrap();
            let fingerprint = format!(
                "{:x}",
                Sha256::digest(stream.get_ref().1.peer_certificates().unwrap()[0].as_ref())
            );
            assert_eq!(fingerprint, expected);
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
                assert!(headers.len() < 16384);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            assert!(length < 16384);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            let response = if index == 0 {
                assert!(headers.starts_with("POST /v1/network/renew "));
                let input: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert!(
                    input["csr_pem"]
                        .as_str()
                        .unwrap()
                        .contains("CERTIFICATE REQUEST")
                );
                assert!(!input["csr_pem"].as_str().unwrap().contains("PRIVATE KEY"));
                serde_json::json!({"hub_id":"hub","device_id":"device-b","label":"Win19","certificate_pem":new_certificate,"ca_certificate_pem":ca,"certificate_sha256":new_fingerprint,"expires_at_ms":expiry})
            } else {
                assert!(headers.starts_with("GET /v1/network/self "));
                serde_json::json!({"hub_id":"hub","device_id":"device-b","label":"Win19","groups":[],"revision":"1","certificate_sha256":new_fingerprint,"expires_at_ms":expiry,"cancelled_lineages":[]})
            };
            let body = serde_json::to_vec(&response).unwrap();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    assert!(service.inner.receiver.lock().await.is_none());
    service.renew_if_needed().await.unwrap();
    let status = existing_owner.self_status().await.unwrap();
    assert_eq!(status.certificate_sha256, expected_fingerprint);
    assert_eq!(status.device_id, "device-b");
    assert_eq!(
        service.inner.settings.load().unwrap().certificate_sha256,
        Some(expected_fingerprint)
    );
    assert_eq!(
        std::fs::read(service.inner.directory.join("identity.json")).unwrap(),
        key_before
    );
    assert!(service.inner.receiver.lock().await.is_none());
    assert!(!service.projection_now().receiver.enabled);
    server.await.unwrap();
    service.shutdown().await;
}
