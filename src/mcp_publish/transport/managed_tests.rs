use super::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const TOKEN: &str = "managed-grant-test-01234567890123456789";
const OTHER: &str = "managed-other-test-01234567890123456789";

struct Pki {
    ca: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}
impl Pki {
    fn new() -> Self {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let ca = params.self_signed(&key).unwrap().pem();
        let issuer = rcgen::Issuer::new(params, key);
        Self { ca, issuer }
    }
    fn leaf(&self) -> (String, String, String) {
        self.leaf_for_names(vec!["127.0.0.1".into()])
    }
    fn leaf_for_names(&self, names: Vec<String>) -> (String, String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(names).unwrap();
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        ];
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        (
            cert.pem(),
            key.serialize_pem(),
            format!("{:x}", Sha256::digest(cert.der().as_ref())),
        )
    }
    fn client(&self, identity: Option<(&str, &str)>) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(self.ca.as_bytes()).unwrap());
        if let Some((cert, key)) = identity {
            builder = builder.identity(
                reqwest::Identity::from_pem(format!("{cert}\n{key}").as_bytes()).unwrap(),
            );
        }
        builder.build().unwrap()
    }
}

struct Auth {
    actor: String,
    revoked: AtomicBool,
    accepting: AtomicBool,
    calls: Mutex<Vec<(String, String)>>,
    sequence: AtomicUsize,
}
#[async_trait]
impl DeviceRequestAuthenticator for Auth {
    async fn authenticate(
        &self,
        token: &str,
        fingerprint: &str,
        action: &str,
    ) -> Result<VerifiedGrant, PublishCallError> {
        self.calls
            .lock()
            .unwrap()
            .push((fingerprint.to_owned(), action.to_owned()));
        if self.revoked.load(Ordering::SeqCst)
            || fingerprint != self.actor
            || ![TOKEN, OTHER].contains(&token)
            || (!self.accepting.load(Ordering::SeqCst) && action == "execute")
        {
            return Err(PublishCallError::Unavailable);
        }
        Ok(VerifiedGrant {
            grant_id: format!("grant-{}", self.sequence.fetch_add(1, Ordering::SeqCst)),
            claims: GrantClaims {
                hub_id: "hub".into(),
                origin_device_id: "A".into(),
                actor_device_id: "A".into(),
                audience_device_id: "B".into(),
                profile_id: "temp".into(),
                mode: "agent".into(),
                scope_id: "scope".into(),
                root_task_id: if token == TOKEN { "root" } else { "other" }.into(),
                request_key: "request".into(),
                parent_job_id: None,
                depth: 1,
                device_path: vec!["A".into(), "B".into()],
            },
        })
    }
}
struct Dispatcher(AtomicUsize);
#[async_trait]
impl PublishToolDispatcher for Dispatcher {
    fn tool_descriptors(&self) -> Vec<Value> {
        ["delegate_task", "task_status", "cancel_task"]
            .into_iter()
            .map(|name| json!({"name":name,"inputSchema":{"type":"object"}}))
            .collect()
    }
    async fn call(
        &self,
        _: &str,
        _: Value,
        _: CancellationToken,
    ) -> Result<Value, PublishCallError> {
        panic!("managed request must never fall back to manual dispatch")
    }
    async fn call_authorized(
        &self,
        _: &str,
        _: Value,
        _: CancellationToken,
        authority: VerifiedGrant,
    ) -> Result<Value, PublishCallError> {
        assert_eq!(authority.claims().actor_device_id, "A");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"content":[{"type":"text","text":"authorized"}],"isError":false}))
    }
}

fn post(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    session: Option<&str>,
    message: Value,
) -> reqwest::RequestBuilder {
    let mut request = client
        .post(endpoint)
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL_VERSION)
        .json(&message);
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    request
}
fn init() -> Value {
    json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
        "protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})
}

#[tokio::test]
async fn managed_hub_tls_rejects_worker_certificates_at_the_same_ip_before_http() {
    let pki = Pki::new();
    let (actor_cert, actor_key, actor) = pki.leaf();
    let client =
        super::super::tls::managed_hub_http(Some((&actor_cert, &actor_key)), &pki.ca).unwrap();
    for (names, expected_hub) in [
        (vec!["127.0.0.1".into()], false),
        (
            vec![
                "127.0.0.1".into(),
                super::super::tls::HUB_TLS_ROLE_NAME.into(),
            ],
            true,
        ),
        (
            vec![
                "127.0.0.2".into(),
                super::super::tls::HUB_TLS_ROLE_NAME.into(),
            ],
            false,
        ),
    ] {
        let (cert, key, _) = pki.leaf_for_names(names);
        let auth = Arc::new(Auth {
            actor: actor.clone(),
            revoked: AtomicBool::new(false),
            accepting: AtomicBool::new(true),
            calls: Mutex::new(vec![]),
            sequence: AtomicUsize::new(0),
        });
        let mut server = PublishHttpServer::start_managed(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(Dispatcher(AtomicUsize::new(0))),
            2,
            super::super::tls::load_mtls_acceptor(&cert, &key, &pki.ca).unwrap(),
            auth.clone(),
        )
        .await
        .unwrap();
        let endpoint = server.endpoint();
        let result = post(&client, &endpoint, TOKEN, None, init()).send().await;
        if expected_hub {
            assert_eq!(result.unwrap().status(), StatusCode::OK);
            assert_eq!(auth.calls.lock().unwrap().len(), 1);
        } else {
            assert!(
                result.is_err(),
                "both the endpoint IP and the reserved Hub role are required"
            );
            assert!(
                auth.calls.lock().unwrap().is_empty(),
                "no HTTP body may reach a role/IP mismatch"
            );
        }
        assert!(server.stop().await);
    }
}

#[tokio::test]
async fn managed_mtls_binds_sessions_and_rechecks_actual_operation_after_policy_changes() {
    let pki = Pki::new();
    let (cert, key, _) = pki.leaf();
    let (actor_cert, actor_key, actor) = pki.leaf();
    let (other_cert, other_key, _) = pki.leaf();
    let auth = Arc::new(Auth {
        actor,
        revoked: AtomicBool::new(false),
        accepting: AtomicBool::new(true),
        calls: Mutex::new(vec![]),
        sequence: AtomicUsize::new(0),
    });
    let dispatcher = Arc::new(Dispatcher(AtomicUsize::new(0)));
    let mut server = PublishHttpServer::start_managed(
        "127.0.0.1:0".parse().unwrap(),
        dispatcher.clone(),
        2,
        super::super::tls::load_mtls_acceptor(&cert, &key, &pki.ca).unwrap(),
        auth.clone(),
    )
    .await
    .unwrap();
    let endpoint = server.endpoint();
    let anonymous = pki.client(None);
    assert!(
        post(&anonymous, &endpoint, TOKEN, None, init())
            .send()
            .await
            .is_err()
    );
    let foreign = Pki::new();
    let (foreign_cert, foreign_key, _) = foreign.leaf();
    let untrusted = pki.client(Some((&foreign_cert, &foreign_key)));
    assert!(
        post(&untrusted, &endpoint, TOKEN, None, init())
            .send()
            .await
            .is_err()
    );
    assert!(auth.calls.lock().unwrap().is_empty());
    let impostor = pki.client(Some((&other_cert, &other_key)));
    assert_eq!(
        post(&impostor, &endpoint, TOKEN, None, init())
            .header("x-client-certificate-sha256", &auth.actor)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let before_pin = auth.calls.lock().unwrap().len();
    let wrong_device =
        super::super::tls::managed_peer_http(&actor_cert, &actor_key, &pki.ca, &other_cert)
            .unwrap();
    assert!(
        post(&wrong_device, &endpoint, TOKEN, None, init())
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        auth.calls.lock().unwrap().len(),
        before_pin,
        "a valid same-CA certificate for another device must fail before HTTP"
    );
    let client =
        super::super::tls::managed_peer_http(&actor_cert, &actor_key, &pki.ca, &cert).unwrap();
    assert_eq!(
        post(&client, &endpoint, TOKEN, None, init())
            .header("origin", endpoint.trim_end_matches("/mcp"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = post(&client, &endpoint, TOKEN, None, init())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        post(
            &client,
            &endpoint,
            TOKEN,
            Some(&session),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::ACCEPTED
    );
    // Rotating credentials with the same claims preserve the session; a new root cannot borrow it.
    let call = |id, name| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}});
    assert_eq!(
        post(
            &client,
            &endpoint,
            OTHER,
            Some(&session),
            call(1, "task_status")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(
            &client,
            &endpoint,
            TOKEN,
            Some(&session),
            call(1, "delegate_task")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    assert_eq!(dispatcher.0.load(Ordering::SeqCst), 1);
    auth.accepting.store(false, Ordering::SeqCst);
    assert_eq!(
        post(
            &client,
            &endpoint,
            TOKEN,
            Some(&session),
            call(2, "delegate_task")
        )
        .header("x-authorization-action", "observe")
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::FORBIDDEN
    );
    for (id, name) in [(3, "task_status"), (4, "cancel_task")] {
        assert_eq!(
            post(&client, &endpoint, TOKEN, Some(&session), call(id, name))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(dispatcher.0.load(Ordering::SeqCst), 3);
    assert_eq!(
        client
            .delete(&endpoint)
            .bearer_auth(OTHER)
            .header("mcp-protocol-version", PROTOCOL_VERSION)
            .header("mcp-session-id", &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    auth.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        post(
            &client,
            &endpoint,
            TOKEN,
            Some(&session),
            call(5, "task_status")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&client, &endpoint, TOKEN, None, init())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(dispatcher.0.load(Ordering::SeqCst), 3);
    let calls = auth.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|(_, action)| action == "cancel"));
    assert!(calls.iter().any(|(_, action)| action == "execute"));
    assert!(server.stop().await);
}
