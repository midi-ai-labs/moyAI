use super::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

const TOKEN: &str = "mcp-publish-test-token-01234567890123456789";

#[test]
fn default_https_authority_matches_normalized_client_host() {
    assert!(host_matches(
        "192.168.10.2:443".parse().unwrap(),
        "https",
        "192.168.10.2"
    ));
    assert!(host_matches("[::1]:443".parse().unwrap(), "https", "[::1]"));
    assert!(host_matches(
        "127.0.0.1:80".parse().unwrap(),
        "http",
        "localhost"
    ));
    assert!(!host_matches(
        "192.168.10.2:8443".parse().unwrap(),
        "https",
        "192.168.10.2"
    ));
    assert!(!host_matches(
        "192.168.10.2:443".parse().unwrap(),
        "https",
        "localhost"
    ));
    assert!(!host_matches(
        "192.168.10.2:443".parse().unwrap(),
        "https",
        "192.168.10.3"
    ));
    assert!(!host_matches(
        "192.168.10.2:443".parse().unwrap(),
        "https",
        "user@192.168.10.2"
    ));
}

#[tokio::test]
async fn tls_peer_trust_and_slow_handshake_are_isolated() {
    use crate::mcp_publish::{PublishProfileId, tls::create_certificate};
    use camino::Utf8PathBuf;
    let temp = tempfile::tempdir().unwrap();
    let directory = Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
    let trusted = create_certificate(
        &directory,
        PublishProfileId(ulid::Ulid::new()),
        "127.0.0.1".parse().unwrap(),
    )
    .unwrap();
    let other = create_certificate(
        &directory,
        PublishProfileId(ulid::Ulid::new()),
        "127.0.0.1".parse().unwrap(),
    )
    .unwrap();
    let dispatcher = FixtureDispatcher::new();
    let mut server = PublishHttpServer::start_with_tls(
        "127.0.0.1:0".parse().unwrap(),
        Sha256::digest(TOKEN.as_bytes()).into(),
        dispatcher.clone(),
        2,
        Some(&trusted.tls),
    )
    .await
    .unwrap();
    // One client which never completes TLS must not block a paired client's initialization.
    let url = reqwest::Url::parse(&server.endpoint()).unwrap();
    let stalled = TcpStream::connect(("127.0.0.1", url.port().unwrap()))
        .await
        .unwrap();
    let connection = |id: &str, pem: String| crate::config::McpServerConfig {
        display_name: None,
        id: id.into(),
        enabled: true,
        transport: crate::config::McpTransportKind::Http,
        base_url: server.endpoint(),
        timeout_ms: 2500,
        remote_agent: false,
        trusted_certificate_pem: Some(pem),
        tool_routes: vec![crate::config::McpToolRouteConfig {
            name: "current_time".into(),
            effect: crate::tool::ToolEffectClass::Read,
        }],
        headers: [("Authorization".into(), format!("Bearer {TOKEN}"))].into(),
    };
    let client = crate::mcp::McpClient::new(crate::config::McpConfig {
        enabled: true,
        servers: vec![
            connection("paired", trusted.certificate_pem.clone()),
            connection("wrong-peer", other.certificate_pem),
        ],
    });
    let reply = client
        .call_tool("paired", "current_time", json!({}), || Ok(()))
        .await
        .unwrap();
    assert!(
        matches!(reply, crate::mcp::McpOperationResult::ToolCalled { ref output_text, .. } if output_text == "fixture time")
    );
    assert!(client.list_tools("wrong-peer", || Ok(())).await.is_err());
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
    drop(stalled);
    assert!(server.stop().await);
}

#[tokio::test]
async fn tls_listener_still_requires_token_and_exact_origin() {
    let temp = tempfile::tempdir().unwrap();
    let directory = camino::Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
    let cert = crate::mcp_publish::tls::create_certificate(
        &directory,
        crate::mcp_publish::PublishProfileId(ulid::Ulid::new()),
        "127.0.0.1".parse().unwrap(),
    )
    .unwrap();
    let mut server = PublishHttpServer::start_with_tls(
        "127.0.0.1:0".parse().unwrap(),
        Sha256::digest(TOKEN.as_bytes()).into(),
        FixtureDispatcher::new(),
        1,
        Some(&cert.tls),
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(
            reqwest::Certificate::from_pem(cert.certificate_pem.as_bytes()).unwrap(),
        )
        .build()
        .unwrap();
    let request = || {
        client
            .post(server.endpoint())
            .header("accept", "application/json, text/event-stream")
            .json(&initialization(1))
    };
    assert_eq!(request().send().await.unwrap().status(), 401);
    assert_eq!(
        request()
            .bearer_auth(TOKEN)
            .header("origin", "http://127.0.0.1")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        request()
            .bearer_auth(TOKEN)
            .header("host", "attacker.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        request().bearer_auth(TOKEN).send().await.unwrap().status(),
        200
    );
    assert!(server.stop().await);
}

struct FixtureDispatcher {
    mode: AtomicU8,
    calls: AtomicUsize,
    started: Notify,
    gate: Semaphore,
    cancelled: AtomicBool,
}

impl FixtureDispatcher {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            mode: AtomicU8::new(0),
            calls: AtomicUsize::new(0),
            started: Notify::new(),
            gate: Semaphore::new(0),
            cancelled: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl PublishToolDispatcher for FixtureDispatcher {
    fn tool_descriptors(&self) -> Vec<Value> {
        vec![
            json!({"name":"current_time","description":"Current time","inputSchema":{"type":"object"}}),
        ]
    }

    async fn call(
        &self,
        _: &str,
        _: Value,
        cancel: CancellationToken,
    ) -> Result<Value, PublishCallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.mode.load(Ordering::SeqCst) == 1 {
            self.started.notify_one();
            tokio::select! {
                _ = cancel.cancelled() => {
                    self.cancelled.store(true, Ordering::SeqCst);
                    self.gate.acquire().await.unwrap().forget();
                }
                permit = self.gate.acquire() => permit.unwrap().forget(),
            }
        }
        if cancel.is_cancelled() {
            return Err(PublishCallError::Cancelled);
        }
        if self.mode.load(Ordering::SeqCst) == 2 {
            return Ok(
                json!({"content":[{"type":"text","text":"X".repeat(MAX_RESPONSE)}],"isError":false}),
            );
        }
        if self.mode.load(Ordering::SeqCst) == 3 {
            return Err(PublishCallError::TargetChanged);
        }
        Ok(json!({"content":[{"type":"text","text":"fixture time"}],"isError":false}))
    }
}

struct Fixture {
    server: PublishHttpServer,
    dispatcher: Arc<FixtureDispatcher>,
    client: reqwest::Client,
}

impl Fixture {
    async fn start() -> Self {
        let dispatcher = FixtureDispatcher::new();
        let server = PublishHttpServer::start(
            "127.0.0.1:0".parse().unwrap(),
            Sha256::digest(TOKEN.as_bytes()).into(),
            dispatcher.clone(),
            1,
        )
        .await
        .unwrap();
        Self {
            server,
            dispatcher,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        }
    }

    fn post(&self, session: Option<&str>, value: Value) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(self.server.endpoint())
            .bearer_auth(TOKEN)
            .header("accept", "application/json, text/event-stream")
            .json(&value);
        if let Some(session) = session {
            request = request
                .header("mcp-session-id", session)
                .header("mcp-protocol-version", PROTOCOL_VERSION);
        }
        request
    }

    async fn session(&self) -> String {
        let response = self.post(None, initialization(1)).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            self.post(
                Some(&session),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"})
            )
            .send()
            .await
            .unwrap()
            .status(),
            202
        );
        session
    }

    fn outbound_client(&self) -> crate::mcp::McpClient {
        crate::mcp::McpClient::new(crate::config::McpConfig {
            enabled: true,
            servers: vec![crate::config::McpServerConfig {
                display_name: None,
                id: "published".into(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: self.server.endpoint(),
                timeout_ms: 2_000,
                remote_agent: false,
                trusted_certificate_pem: None,
                tool_routes: vec![crate::config::McpToolRouteConfig {
                    name: "current_time".into(),
                    effect: crate::tool::ToolEffectClass::Read,
                }],
                headers: [("Authorization".into(), format!("Bearer {TOKEN}"))].into(),
            }],
        })
    }
}

fn initialization(id: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
        "protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}
    }})
}

fn call(id: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"current_time","arguments":{}}})
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn initialization_version_negotiation_and_tool_lifecycle_use_real_http() {
    let mut fixture = Fixture::start().await;
    let mut initialize = initialization(1);
    initialize["params"]["protocolVersion"] = json!("2024-11-05");
    let response = fixture.post(None, initialize).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(
        value["result"]["capabilities"],
        json!({"tools":{"listChanged":false}})
    );
    assert!(value["result"].get("sampling").is_none());

    let response = fixture
        .post(
            Some(&session),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        fixture
            .post(
                Some(&session),
                json!({"jsonrpc":"2.0","id":3,"method":"ping"})
            )
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["result"],
        json!({})
    );
    let notification = fixture
        .post(
            Some(&session),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(notification.status(), 202);
    assert!(notification.bytes().await.unwrap().is_empty());
    let listed: Value = fixture
        .post(
            Some(&session),
            json!({"jsonrpc":"2.0","id":"list","method":"tools/list"}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["id"], "list");
    assert_eq!(listed["result"]["tools"][0]["name"], "current_time");
    let result: Value = fixture
        .post(Some(&session), call(4))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["result"]["isError"], false);
    assert_eq!(
        fixture.server.snapshot().recent_calls[0].status,
        "completed"
    );
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn credentials_host_origin_and_protocol_headers_are_checked_before_dispatch() {
    let mut fixture = Fixture::start().await;
    let session = fixture.session().await;
    for method in [
        reqwest::Method::POST,
        reqwest::Method::GET,
        reqwest::Method::DELETE,
    ] {
        let response = fixture
            .client
            .request(method, fixture.server.endpoint())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert!(response.headers().contains_key("www-authenticate"));
        assert!(!response.text().await.unwrap().contains(TOKEN));
    }
    for (name, value, status) in [
        (
            "authorization",
            "Bearer wrong-token-01234567890123456789012",
            401,
        ),
        ("host", "attacker.example:7332", 403),
        ("origin", "https://attacker.example", 403),
        ("origin", "null", 403),
        ("mcp-protocol-version", "2026-07-28", 400),
        ("content-type", "text/plain", 415),
        ("accept", "application/json", 406),
    ] {
        assert_eq!(
            fixture
                .post(Some(&session), call(2))
                .header(name, value)
                .send()
                .await
                .unwrap()
                .status(),
            status,
            "{name}"
        );
    }
    let origin = fixture
        .server
        .endpoint()
        .trim_end_matches("/mcp")
        .to_owned();
    assert_eq!(
        fixture
            .post(Some(&session), call(2))
            .header("origin", origin)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .client
            .get(fixture.server.endpoint())
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn malformed_envelopes_missing_sessions_and_notifications_never_invoke_tools() {
    let mut fixture = Fixture::start().await;
    let session = fixture.session().await;
    assert_eq!(
        fixture.post(None, call(2)).send().await.unwrap().status(),
        400
    );
    assert_eq!(
        fixture
            .post(Some("unknown-session"), call(2))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    for envelope in [
        json!([]),
        json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":1.5,"method":"ping"}),
        json!({"jsonrpc":"1.0","id":2,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":2,"method":"ping","params":[]}),
    ] {
        assert_eq!(
            fixture
                .post(Some(&session), envelope)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let response = fixture
        .post(Some(&session), json!({}))
        .body("{")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        -32700
    );
    for method in ["notifications/unknown", "tools/call"] {
        assert_eq!(
            fixture
                .post(
                    Some(&session),
                    json!({"jsonrpc":"2.0","method":method,"params":{"name":"current_time"}})
                )
                .send()
                .await
                .unwrap()
                .status(),
            202
        );
    }
    let invalid: Value = fixture
        .post(
            Some(&session),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"not_published"}}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(invalid["error"]["code"], -32602);
    let unknown: Value = fixture
        .post(
            Some(&session),
            json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unknown["error"]["code"], -32601);
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 0);
    assert!(fixture.server.snapshot().recent_calls.is_empty());
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn request_response_and_header_limits_return_bounded_nonsecret_errors() {
    let mut fixture = Fixture::start().await;
    let session = fixture.session().await;
    assert_eq!(
        fixture
            .post(Some(&session), call(2))
            .body(" ".repeat(MAX_BODY + 1))
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    assert_eq!(
        fixture
            .post(Some(&session), call(2))
            .header("x-large", "x".repeat(MAX_HEADERS))
            .send()
            .await
            .unwrap()
            .status(),
        431
    );
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 0);
    fixture.dispatcher.mode.store(2, Ordering::SeqCst);
    let response = fixture.post(Some(&session), call(2)).send().await.unwrap();
    let body = response.bytes().await.unwrap();
    assert!(body.len() < 256);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["id"], 2);
    assert_eq!(body["error"]["code"], -32603);
    fixture.dispatcher.mode.store(3, Ordering::SeqCst);
    let response = fixture
        .post(Some(&session), call(3))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(response.contains("Tool target unavailable"));
    assert!(!response.contains(TOKEN));
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn cancellation_is_session_scoped_and_capacity_is_held_until_worker_settlement() {
    let mut fixture = Fixture::start().await;
    let first = fixture.session().await;
    let second = fixture.session().await;
    fixture.dispatcher.mode.store(1, Ordering::SeqCst);
    let request = fixture.post(Some(&first), call(2));
    let task = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.dispatcher.started.notified(),
    )
    .await
    .unwrap();
    let duplicate: Value = fixture
        .post(Some(&first), call(2))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(duplicate["error"]["code"], -32600);
    assert_eq!(
        fixture
            .post(Some(&second), call(2))
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    let cancel = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"private reason"}});
    assert_eq!(
        fixture
            .post(Some(&second), cancel.clone())
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    assert!(!fixture.dispatcher.cancelled.load(Ordering::SeqCst));
    assert_eq!(
        fixture
            .post(Some(&first), cancel)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    until(|| fixture.dispatcher.cancelled.load(Ordering::SeqCst)).await;
    assert_eq!(task.await.unwrap().status(), 202);
    let snapshot = fixture.server.snapshot();
    assert_eq!(snapshot.active_calls, 1);
    assert_eq!(snapshot.recent_calls[0].status, "cancelling");
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("private reason")
    );
    fixture.dispatcher.gate.add_permits(1);
    until(|| fixture.server.snapshot().active_calls == 0).await;
    assert_eq!(
        fixture.server.snapshot().recent_calls[0].status,
        "cancelled"
    );
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .post(Some(&first), call(2))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn downstream_disconnect_does_not_cancel_a_tool_but_stop_invalidates_and_drains() {
    let mut fixture = Fixture::start().await;
    let session = fixture.session().await;
    let observer = fixture.server.observation();
    fixture.dispatcher.mode.store(1, Ordering::SeqCst);
    let request = fixture.post(Some(&session), call(2));
    let task = tokio::spawn(async move { request.send().await });
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.dispatcher.started.notified(),
    )
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!fixture.dispatcher.cancelled.load(Ordering::SeqCst));
    assert_eq!(observer.snapshot().active_calls, 1);
    assert!(
        !fixture.server.stop().await,
        "blocked worker prevents false clean stop"
    );
    let snapshot = observer.snapshot();
    assert!(!snapshot.accepting);
    assert!(snapshot.live);
    assert_eq!(snapshot.sessions, 0);
    assert_eq!(snapshot.active_calls, 1);
    assert!(fixture.dispatcher.cancelled.load(Ordering::SeqCst));
    fixture.dispatcher.gate.add_permits(1);
    assert!(fixture.server.stop().await);
    assert!(!observer.snapshot().live);
    assert_eq!(observer.snapshot().active_calls, 0);
}

#[tokio::test]
async fn session_delete_idle_expiry_and_capacity_require_fresh_initialization() {
    let mut fixture = Fixture::start().await;
    let session = fixture.session().await;
    let response = fixture
        .client
        .delete(fixture.server.endpoint())
        .bearer_auth(TOKEN)
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", PROTOCOL_VERSION)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    assert_eq!(
        fixture
            .post(Some(&session), call(2))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let expired = fixture.session().await;
    fixture
        .server
        .shared
        .sessions
        .lock()
        .unwrap()
        .get_mut(&expired)
        .unwrap()
        .touched = Instant::now() - SESSION_IDLE;
    assert_eq!(
        fixture
            .post(Some(&expired), call(2))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    for _ in 0..MAX_SESSIONS {
        fixture.session().await;
    }
    assert_eq!(
        fixture
            .post(None, initialization(1))
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(fixture.server.snapshot().sessions, MAX_SESSIONS);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn audit_is_bounded_and_contains_no_arguments_or_caller_request_ids() {
    let mut fixture = Fixture::start().await;
    let session = fixture.session().await;
    for id in 2..(MAX_RECENT_CALLS + 4) {
        let mut request = call(id as u64);
        request["id"] = json!(format!("private-id-{id}"));
        request["params"]["arguments"] = json!({"private":"caller body secret"});
        assert_eq!(
            fixture
                .post(Some(&session), request)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    let snapshot = fixture.server.snapshot();
    assert_eq!(snapshot.recent_calls.len(), MAX_RECENT_CALLS);
    assert_eq!(snapshot.active_calls, 0);
    let serialized = serde_json::to_string(&snapshot).unwrap();
    for secret in [TOKEN, "caller body secret", "private-id", "arguments"] {
        assert!(!serialized.contains(secret));
    }
    assert!(
        snapshot
            .recent_calls
            .iter()
            .all(|record| record.tool == "current_time" && record.status == "completed")
    );
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn idle_tcp_connections_consume_bounded_permits_and_release_on_drop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let slots = Arc::new(Semaphore::new(1));
    let mut listener = BoundedListener {
        listener,
        slots: slots.clone(),
        acceptor: None,
        handshakes: FuturesUnordered::new(),
    };
    let _first_client = TcpStream::connect(address).await.unwrap();
    let (first, _) = axum::serve::Listener::accept(&mut listener).await;
    assert_eq!(slots.available_permits(), 0);
    let mut second_client = TcpStream::connect(address).await.unwrap();
    second_client
        .write_all(b"GET /mcp HTTP/1.1\r\n")
        .await
        .unwrap();
    let next_accept =
        tokio::spawn(async move { axum::serve::Listener::accept(&mut listener).await });
    let mut byte = [0];
    let read = tokio::time::timeout(Duration::from_secs(2), second_client.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)));
    drop(first);
    assert_eq!(slots.available_permits(), 1);
    next_accept.abort();
    let _ = next_accept.await;
}

#[tokio::test]
async fn existing_outbound_client_negotiates_publish_server_and_reuses_session() {
    let mut fixture = Fixture::start().await;
    let client = fixture.outbound_client();
    for _ in 0..2 {
        let listed = client.list_tools("published", || Ok(())).await.unwrap();
        assert!(
            matches!(listed, crate::mcp::McpOperationResult::ToolsListed { tools, .. } if tools.len() == 1)
        );
        let result = client
            .call_tool("published", "current_time", json!({}), || Ok(()))
            .await
            .unwrap();
        assert!(
            matches!(result, crate::mcp::McpOperationResult::ToolCalled { output_text, .. } if output_text == "fixture time")
        );
    }
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.server.snapshot().sessions, 1);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn outbound_client_never_replays_an_expired_session_tool_call() {
    let mut fixture = Fixture::start().await;
    let client = fixture.outbound_client();
    client
        .call_tool("published", "current_time", json!({}), || Ok(()))
        .await
        .unwrap();
    for session in fixture.server.shared.sessions.lock().unwrap().values_mut() {
        session.touched = Instant::now() - SESSION_IDLE;
    }
    let failed = client
        .call_tool("published", "current_time", json!({}), || Ok(()))
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("404"));
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 1);
    client
        .call_tool("published", "current_time", json!({}), || Ok(()))
        .await
        .unwrap();
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.server.snapshot().sessions, 1);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn concurrent_outbound_discovery_initializes_one_session() {
    let mut fixture = Fixture::start().await;
    let client = fixture.outbound_client();
    let other = client.clone();
    let (first, second) = tokio::join!(
        client.list_tools("published", || Ok(())),
        other.list_tools("published", || Ok(())),
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(fixture.server.snapshot().sessions, 1);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn outbound_initialization_rechecks_effect_admission_before_every_send() {
    let mut fixture = Fixture::start().await;
    let client = fixture.outbound_client();
    let mut sends = 0;
    let error = client
        .list_tools("published", || {
            sends += 1;
            if sends == 1 {
                Ok(())
            } else {
                Err(crate::error::ToolError::RunInterrupted)
            }
        })
        .await
        .unwrap_err();
    assert!(matches!(error, crate::error::ToolError::RunInterrupted));
    assert_eq!(sends, 2);
    assert_eq!(fixture.server.snapshot().sessions, 0);
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 0);
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn request_id_budget_terminates_session_and_cancels_without_releasing_active_work() {
    let mut fixture = Fixture::start().await;
    let session_id = fixture.session().await;
    fixture.dispatcher.mode.store(1, Ordering::SeqCst);
    let request = fixture.post(Some(&session_id), call(2));
    let task = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.dispatcher.started.notified(),
    )
    .await
    .unwrap();
    {
        let mut sessions = fixture.server.shared.sessions.lock().unwrap();
        let session = sessions.get_mut(&session_id).unwrap();
        for index in session.used_ids.len()..MAX_REQUEST_IDS {
            session.used_ids.insert(format!("budget:{index}"));
        }
    }
    let exhausted = fixture
        .post(
            Some(&session_id),
            json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(exhausted.status(), 404);
    until(|| fixture.dispatcher.cancelled.load(Ordering::SeqCst)).await;
    assert_eq!(task.await.unwrap().status(), 202);
    assert_eq!(fixture.server.snapshot().sessions, 0);
    assert_eq!(fixture.server.snapshot().active_calls, 1);
    assert_eq!(
        fixture.server.snapshot().recent_calls[0].status,
        "cancelling"
    );
    fixture.dispatcher.gate.add_permits(1);
    until(|| fixture.server.snapshot().active_calls == 0).await;
    assert!(fixture.server.stop().await);
}

#[tokio::test]
async fn outbound_request_budget_refusal_is_not_replayed_and_next_explicit_call_recovers() {
    let mut fixture = Fixture::start().await;
    let client = fixture.outbound_client();
    client
        .call_tool("published", "current_time", json!({}), || Ok(()))
        .await
        .unwrap();
    {
        let mut sessions = fixture.server.shared.sessions.lock().unwrap();
        for session in sessions.values_mut() {
            for index in session.used_ids.len()..MAX_REQUEST_IDS {
                session.used_ids.insert(format!("budget:{index}"));
            }
        }
    }
    let error = client
        .call_tool("published", "current_time", json!({}), || Ok(()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("404"));
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.server.snapshot().sessions, 0);
    client
        .call_tool("published", "current_time", json!({}), || Ok(()))
        .await
        .unwrap();
    assert_eq!(fixture.dispatcher.calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.server.snapshot().sessions, 1);
    assert!(fixture.server.stop().await);
}
