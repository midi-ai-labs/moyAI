use super::*;
use crate::config::{McpToolRouteConfig, McpTransportKind};
use crate::device_network::{DirectoryPeer, GrantClaims, VerifiedGrant};
use crate::mcp_publish::dispatch::{
    DeviceRequestAuthenticator, PublishCallError, PublishToolDispatcher,
};
use crate::mcp_publish::transport::PublishHttpServer;
use crate::tool::ToolEffectClass;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use tokio_util::sync::CancellationToken;

struct Auth {
    actor: String,
    grants: Mutex<HashMap<String, GrantClaims>>,
    observed: Mutex<Vec<(String, String)>>,
    revoked: AtomicBool,
}
#[async_trait]
impl DeviceRequestAuthenticator for Auth {
    async fn authenticate(
        &self,
        token: &str,
        actor: &str,
        action: &str,
    ) -> Result<VerifiedGrant, PublishCallError> {
        if actor != self.actor || self.revoked.load(Ordering::SeqCst) {
            return Err(PublishCallError::Unavailable);
        }
        let claims = self
            .grants
            .lock()
            .unwrap()
            .get(token)
            .cloned()
            .ok_or(PublishCallError::Unavailable)?;
        self.observed
            .lock()
            .unwrap()
            .push((token.into(), action.into()));
        Ok(VerifiedGrant {
            grant_id: "fresh-introspection".into(),
            claims,
        })
    }
}

struct Dispatcher {
    calls: AtomicUsize,
    fail: AtomicBool,
}
#[async_trait]
impl PublishToolDispatcher for Dispatcher {
    fn tool_descriptors(&self) -> Vec<Value> {
        vec![
            json!({"name":"delegate_task","inputSchema":{"type":"object"}}),
            json!({"name":"task_status","inputSchema":{"type":"object"}}),
        ]
    }
    async fn call(
        &self,
        _: &str,
        _: Value,
        _: CancellationToken,
    ) -> Result<Value, PublishCallError> {
        panic!("managed dispatch requires fresh authorization")
    }
    async fn call_authorized(
        &self,
        _: &str,
        _: Value,
        _: CancellationToken,
        authority: VerifiedGrant,
    ) -> Result<Value, PublishCallError> {
        assert_eq!(authority.claims().actor_device_id, "A");
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(PublishCallError::Unavailable);
        }
        Ok(json!({"content":[{"type":"text","text":"ready 日本語"}],"isError":false}))
    }
}

struct Fixture {
    server: PublishHttpServer,
    auth: Arc<Auth>,
    dispatcher: Arc<Dispatcher>,
    ca: String,
    actor_cert: String,
    actor_key: String,
    peer_cert: String,
    peer_fingerprint: String,
}
impl Fixture {
    async fn start() -> Self {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::default();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let ca = ca_params.self_signed(&ca_key).unwrap().pem();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf = || {
            let key = rcgen::KeyPair::generate().unwrap();
            let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
            params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![
                rcgen::ExtendedKeyUsagePurpose::ClientAuth,
                rcgen::ExtendedKeyUsagePurpose::ServerAuth,
            ];
            let cert = params.signed_by(&key, &issuer).unwrap();
            (
                cert.pem(),
                key.serialize_pem(),
                format!("{:x}", Sha256::digest(cert.der().as_ref())),
            )
        };
        let (actor_cert, actor_key, actor) = leaf();
        let (peer_cert, peer_key, peer_fingerprint) = leaf();
        let auth = Arc::new(Auth {
            actor,
            grants: Mutex::new(HashMap::new()),
            observed: Mutex::new(vec![]),
            revoked: AtomicBool::new(false),
        });
        let dispatcher = Arc::new(Dispatcher {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let server = PublishHttpServer::start_managed(
            "127.0.0.1:0".parse().unwrap(),
            dispatcher.clone(),
            4,
            crate::mcp_publish::tls::load_mtls_acceptor(&peer_cert, &peer_key, &ca).unwrap(),
            auth.clone(),
        )
        .await
        .unwrap();
        Self {
            server,
            auth,
            dispatcher,
            ca,
            actor_cert,
            actor_key,
            peer_cert,
            peer_fingerprint,
        }
    }
    fn grant(&self, root: &str, sequence: usize) -> DeviceGrant {
        let claims = GrantClaims {
            hub_id: "hub".into(),
            origin_device_id: "A".into(),
            actor_device_id: "A".into(),
            audience_device_id: "B".into(),
            profile_id: "temp".into(),
            mode: "agent".into(),
            scope_id: "scope".into(),
            root_task_id: root.into(),
            request_key: format!("request-{root}"),
            parent_job_id: None,
            depth: 1,
            device_path: vec!["A".into(), "B".into()],
        };
        let token = format!("managed-fresh-grant-01234567890123456789-{root}-{sequence}");
        self.auth
            .grants
            .lock()
            .unwrap()
            .insert(token.clone(), claims.clone());
        DeviceGrant {
            grant_id: format!("grant-{sequence}"),
            token,
            expires_at_ms: u64::MAX,
            claims,
            peer: DirectoryPeer {
                device_id: "B".into(),
                label: "Win B".into(),
                profile_id: "temp".into(),
                name: "temp".into(),
                endpoint: self.server.endpoint(),
                mode: "agent".into(),
                scope_id: "scope".into(),
                certificate_pem: self.peer_cert.clone(),
                certificate_sha256: self.peer_fingerprint.clone(),
            },
        }
    }
    fn key(&self, grant: &DeviceGrant) -> PeerConnectionKey {
        PeerConnectionKey::new(
            grant,
            "https://hub.invalid/",
            &self.ca,
            &self.actor_cert,
            &self.actor_key,
        )
        .unwrap()
    }
    fn http(&self) -> Result<reqwest::Client, ToolError> {
        crate::mcp_publish::tls::managed_peer_http(
            &self.actor_cert,
            &self.actor_key,
            &self.ca,
            &self.peer_cert,
        )
        .map_err(|_| unavailable())
    }
    fn config(&self) -> McpServerConfig {
        config(self.server.endpoint())
    }
    async fn call(
        &self,
        pool: &PeerConnections,
        grant: &DeviceGrant,
        name: Option<&str>,
    ) -> Result<McpOperationResult, ToolError> {
        pool.operate(
            self.key(grant),
            self.config(),
            &grant.token,
            || self.http(),
            name,
            json!({}),
            || Ok(()),
        )
        .await
    }
}
fn config(endpoint: String) -> McpServerConfig {
    McpServerConfig {
        id: "managed-peer".into(),
        display_name: None,
        enabled: true,
        transport: McpTransportKind::Http,
        base_url: endpoint,
        timeout_ms: 3000,
        remote_agent: true,
        trusted_certificate_pem: None,
        headers: Default::default(),
        tool_routes: vec![
            McpToolRouteConfig {
                name: "delegate_task".into(),
                effect: ToolEffectClass::Destructive,
            },
            McpToolRouteConfig {
                name: "task_status".into(),
                effect: ToolEffectClass::Read,
            },
        ],
    }
}

#[tokio::test]
async fn managed_status_reuses_one_session_beyond_receiver_capacity_with_fresh_authorization() {
    let mut fixture = Fixture::start().await;
    let pool = PeerConnections::default();
    for index in 0..40 {
        // Old tokens are deliberately no longer accepted. A cached bearer
        // header would fail as soon as the operation credential changes.
        fixture.auth.grants.lock().unwrap().clear();
        let grant = fixture.grant("same-task", index);
        fixture
            .call(&pool, &grant, Some("task_status"))
            .await
            .unwrap();
        assert_eq!(fixture.server.snapshot().sessions, 1);
        assert_eq!(
            fixture.auth.observed.lock().unwrap().last().unwrap().0,
            grant.token
        );
    }
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 40);
    assert_eq!(
        fixture
            .auth
            .observed
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, action)| action == "inspect")
            .count(),
        4
    );
    fixture.auth.revoked.store(true, Ordering::SeqCst);
    let grant = fixture.grant("same-task", 41);
    assert!(
        fixture
            .call(&pool, &grant, Some("delegate_task"))
            .await
            .is_err()
    );
    assert_eq!(
        fixture.dispatcher.calls.load(Ordering::SeqCst),
        40,
        "reused connection does not bypass revocation"
    );
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn managed_parallel_first_operations_initialize_once_and_keep_operation_credentials() {
    let mut fixture = Fixture::start().await;
    let pool = PeerConnections::default();
    let first = fixture.grant("same-task", 1);
    let second = fixture.grant("same-task", 2);
    let (a, b) = tokio::join!(
        fixture.call(&pool, &first, Some("delegate_task")),
        fixture.call(&pool, &second, Some("task_status"))
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(fixture.server.snapshot().sessions, 1);
    let observed = fixture.auth.observed.lock().unwrap();
    assert_eq!(
        observed
            .iter()
            .filter(|(_, action)| action == "inspect")
            .count(),
        4
    );
    assert!(observed.contains(&(first.token.clone(), "execute".into())));
    assert!(observed.contains(&(second.token.clone(), "observe".into())));
    drop(observed);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn managed_inspection_and_terminal_cleanup_do_not_exhaust_sessions_or_share_authority() {
    let mut fixture = Fixture::start().await;
    let pool = PeerConnections::default();
    for index in 0..40 {
        let inspect = fixture.grant(&format!("inspect-{index}"), index);
        fixture.call(&pool, &inspect, None).await.unwrap();
        assert_eq!(fixture.server.snapshot().sessions, 1);
        pool.retire(fixture.key(&inspect), &inspect.token, || Ok(()))
            .await
            .unwrap();
        assert_eq!(fixture.server.snapshot().sessions, 0);
    }
    let inspect = fixture.grant("inspect", 50);
    let execute = fixture.grant("execute", 51);
    fixture.call(&pool, &inspect, None).await.unwrap();
    fixture
        .call(&pool, &execute, Some("delegate_task"))
        .await
        .unwrap();
    assert_ne!(fixture.key(&inspect), fixture.key(&execute));
    assert_eq!(
        fixture.server.snapshot().sessions,
        2,
        "different request authority never borrows inspection session"
    );
    pool.retire(fixture.key(&inspect), &inspect.token, || Ok(()))
        .await
        .unwrap();
    pool.retire(fixture.key(&execute), &execute.token, || Ok(()))
        .await
        .unwrap();
    assert_eq!(fixture.server.snapshot().sessions, 0);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn managed_failure_invalidates_only_its_authority_and_never_replays_a_tool_call() {
    let mut fixture = Fixture::start().await;
    let pool = PeerConnections::default();
    let first = fixture.grant("first", 1);
    let second = fixture.grant("second", 2);
    fixture
        .call(&pool, &first, Some("task_status"))
        .await
        .unwrap();
    fixture
        .call(&pool, &second, Some("task_status"))
        .await
        .unwrap();
    fixture.dispatcher.fail.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .call(&pool, &first, Some("delegate_task"))
            .await
            .is_err()
    );
    assert_eq!(
        fixture.dispatcher.calls.load(Ordering::SeqCst),
        3,
        "failed call is not retried"
    );
    assert_eq!(fixture.server.snapshot().sessions, 1);
    let inspect_before = fixture
        .auth
        .observed
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, action)| action == "inspect")
        .count();
    fixture
        .call(&pool, &second, Some("task_status"))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .auth
            .observed
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, action)| action == "inspect")
            .count(),
        inspect_before
    );
    fixture
        .call(&pool, &first, Some("task_status"))
        .await
        .unwrap();
    assert_eq!(fixture.server.snapshot().sessions, 2);
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 5);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn managed_connection_keys_bind_identity_trust_endpoint_and_all_claims_not_rotating_tokens() {
    let mut fixture = Fixture::start().await;
    let base = fixture.grant("task", 1);
    let key = fixture.key(&base);
    let mut rotated = fixture.grant("task", 2);
    rotated.expires_at_ms = 123;
    assert_eq!(fixture.key(&rotated), key);
    let mutate: [fn(&mut DeviceGrant); 15] = [
        |g| g.claims.hub_id.push('2'),
        |g| g.claims.origin_device_id.push('2'),
        |g| g.claims.actor_device_id.push('2'),
        |g| g.claims.audience_device_id.push('2'),
        |g| g.claims.profile_id.push('2'),
        |g| g.claims.mode.push('2'),
        |g| g.claims.scope_id.push('2'),
        |g| g.claims.root_task_id.push('2'),
        |g| g.claims.request_key.push('2'),
        |g| g.claims.parent_job_id = Some("parent".into()),
        |g| g.claims.depth += 1,
        |g| g.claims.device_path.push("C".into()),
        |g| g.peer.endpoint.push_str("/other"),
        |g| g.peer.certificate_pem.push('\n'),
        |g| g.peer.certificate_sha256.push('2'),
    ];
    for change in mutate {
        let mut changed = base.clone();
        change(&mut changed);
        assert_ne!(fixture.key(&changed), key);
    }
    let values = [
        "https://hub.invalid/",
        &fixture.ca,
        &fixture.actor_cert,
        &fixture.actor_key,
    ];
    for index in 0..4 {
        let mut changed = values.map(str::to_owned);
        changed[index].push('2');
        assert_ne!(
            PeerConnectionKey::new(&base, &changed[0], &changed[1], &changed[2], &changed[3])
                .unwrap(),
            key
        );
    }
    assert_eq!(format!("{key:?}"), "PeerConnectionKey([redacted])");
    assert!(fixture.server.stop().await);
}

#[test]
fn managed_cache_capacity_preserves_leased_owners_and_clear_retires_old_leases() {
    let pool = PeerConnections::default();
    let mut leases = (0..MAX_CONNECTIONS)
        .map(|index| pool.entry(PeerConnectionKey([index as u8; 32])).unwrap())
        .collect::<Vec<_>>();
    assert!(pool.entry(PeerConnectionKey([255; 32])).is_err());
    assert!(Arc::ptr_eq(
        &leases[0],
        &pool.entry(PeerConnectionKey([0; 32])).unwrap()
    ));
    leases.remove(1);
    let newest = pool.entry(PeerConnectionKey([255; 32])).unwrap();
    assert_eq!(pool.entries.lock().unwrap().len(), MAX_CONNECTIONS);
    pool.clear();
    assert!(newest.retired.load(Ordering::Acquire));
    assert!(
        leases
            .iter()
            .all(|lease| lease.retired.load(Ordering::Acquire))
    );
    let replacement = pool.entry(PeerConnectionKey([0; 32])).unwrap();
    assert!(!Arc::ptr_eq(&replacement, &leases[0]));
}

#[tokio::test]
async fn managed_ambiguous_http_failure_is_not_resent_and_only_next_operation_reconnects() {
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    let calls = Arc::new(AtomicUsize::new(0));
    let lists = Arc::new(AtomicUsize::new(0));
    let observed_calls = calls.clone();
    let observed_lists = lists.clone();
    let app = Router::new().route("/mcp", post(move |Json(message): Json<Value>| {
        let calls = observed_calls.clone();
        let lists = observed_lists.clone();
        async move {
            if message["method"] == "tools/list" {
                lists.fetch_add(1, Ordering::SeqCst);
                Json(json!({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"delegate_task","inputSchema":{"type":"object"}}]}})).into_response()
            } else {
                // The receiver has performed an effect before its response is
                // lost/replaced by an HTTP error. This operation may not replay.
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (StatusCode::BAD_GATEWAY, "ambiguous remote effect").into_response();
                }
                Json(json!({"jsonrpc":"2.0","id":message["id"],"result":{"content":[],"isError":false}})).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let pool = PeerConnections::default();
    let key = PeerConnectionKey([1; 32]);
    let make_http = || {
        Ok(reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap())
    };
    let call = || {
        pool.operate(
            key,
            config(endpoint.clone()),
            "fresh-test-grant-01234567890123456789",
            make_http,
            Some("delegate_task"),
            json!({}),
            || Ok(()),
        )
    };
    assert!(call().await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(lists.load(Ordering::SeqCst), 1);
    call().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(lists.load(Ordering::SeqCst), 2);
    task.abort();
}
