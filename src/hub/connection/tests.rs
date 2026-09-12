use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tokio::sync::{Notify, Semaphore};

const BOOTSTRAP: &str = "bootstrap-secret-01234567890123456789";

struct ServerState {
    supports_delegated: AtomicBool,
    prepare_response: std::sync::Mutex<Value>,
    prepares: std::sync::Mutex<Vec<Value>>,
    block_prepare: AtomicBool,
    prepare_started: Notify,
    prepare_gate: Semaphore,
    closes: std::sync::Mutex<Vec<Value>>,
    block_close: AtomicBool,
    close_started: Notify,
    close_gate: Semaphore,
    closes_completed: AtomicUsize,
    gateway_requests: std::sync::Mutex<Vec<(Value, String, bool)>>,
    responses_stream: std::sync::Mutex<String>,
    reject_gateway_review: AtomicBool,
    hub_id: std::sync::Mutex<String>,
    revision: AtomicU64,
    registrations: AtomicUsize,
    heartbeats: AtomicUsize,
    activities: std::sync::Mutex<Vec<String>>,
    heartbeat_turns: std::sync::Mutex<Vec<Value>>,
    catalogs: AtomicUsize,
    block_catalog: AtomicBool,
    catalog_started: Notify,
    catalog_gate: Semaphore,
    sessions: std::sync::Mutex<BTreeMap<String, String>>,
    reviews: std::sync::Mutex<BTreeMap<(String, String), Value>>,
    reject_review: AtomicBool,
    reject_heartbeat: AtomicBool,
    block_registration: AtomicBool,
    registration_started: Notify,
    registration_gate: Semaphore,
    block_main_review: AtomicBool,
    review_started: Notify,
    review_gate: Semaphore,
    block_heartbeat: AtomicBool,
    heartbeat_started: Notify,
    heartbeat_gate: Semaphore,
    heartbeat_interval_ms: u64,
}

struct Server {
    endpoint: String,
    state: Arc<ServerState>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn bearer(headers: &HeaderMap) -> &str {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
fn authenticated(state: &ServerState, headers: &HeaderMap, id: &str) -> bool {
    state
        .sessions
        .lock()
        .unwrap()
        .get(id)
        .is_some_and(|token| bearer(headers) == format!("Bearer {token}"))
}
fn token(id: &str) -> String {
    format!("client-token-01234567890123456789-{id}")
}
fn denied() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"unauthorized"})),
    )
}

async fn register(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if bearer(&headers) != format!("Bearer {BOOTSTRAP}") {
        return denied();
    }
    assert!(!body["label"].as_str().unwrap().is_empty());
    let id = format!(
        "desktop-{}",
        state.registrations.fetch_add(1, Ordering::SeqCst)
    );
    let token = token(&id);
    state
        .sessions
        .lock()
        .unwrap()
        .insert(id.clone(), token.clone());
    if state.block_registration.load(Ordering::SeqCst) {
        state.registration_started.notify_one();
        state.registration_gate.acquire().await.unwrap().forget();
    }
    (
        StatusCode::OK,
        Json(
            json!({"id":id,"client_token":token,"hub_id":*state.hub_id.lock().unwrap(),
        "revision":state.revision.load(Ordering::SeqCst).to_string(),
        "identity_scope":"server_session","heartbeat_interval_ms":state.heartbeat_interval_ms,"supports_turn_heartbeat":true,
        "supports_delegated_execution":state.supports_delegated.load(Ordering::SeqCst)}),
        ),
    )
}
async fn catalog(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if !state
        .sessions
        .lock()
        .unwrap()
        .values()
        .any(|token| bearer(&headers) == format!("Bearer {token}"))
    {
        return denied();
    }
    state.catalogs.fetch_add(1, Ordering::SeqCst);
    if state.block_catalog.load(Ordering::SeqCst) {
        state.catalog_started.notify_one();
        state.catalog_gate.acquire().await.unwrap().forget();
    }
    (
        StatusCode::OK,
        Json(
            json!({"hub_id":*state.hub_id.lock().unwrap(), "software_version":"0.1.0",
        "revision":state.revision.load(Ordering::SeqCst).to_string(),"changes":[],
        "models":[{"id":"fast","label":"Fast","capabilities":["chat"]},
        {"id":"deep","label":"Deep","capabilities":["chat"]}]}),
        ),
    )
}
async fn heartbeat(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    state.heartbeats.fetch_add(1, Ordering::SeqCst);
    if state.reject_heartbeat.load(Ordering::SeqCst)
        || !authenticated(&state, &headers, body["id"].as_str().unwrap())
    {
        return denied();
    }
    let activity = body["activity"].as_str().unwrap();
    assert!(matches!(activity, "idle" | "waiting" | "running"));
    state.activities.lock().unwrap().push(activity.into());
    state
        .heartbeat_turns
        .lock()
        .unwrap()
        .push(body["active_turns"].clone());
    let revision = state.revision.load(Ordering::SeqCst).to_string();
    if state.block_heartbeat.load(Ordering::SeqCst) {
        state.heartbeat_started.notify_one();
        state.heartbeat_gate.acquire().await.unwrap().forget();
    }
    (
        StatusCode::OK,
        Json(json!({"id":body["id"],"revision":revision})),
    )
}
async fn review(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let id = body["id"].as_str().unwrap();
    if !authenticated(&state, &headers, id) {
        return denied();
    }
    if state.reject_review.load(Ordering::SeqCst) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"review_required"})),
        );
    }
    if body["context"] == "main" && state.block_main_review.load(Ordering::SeqCst) {
        state.review_started.notify_one();
        state.review_gate.acquire().await.unwrap().forget();
    }
    state.reviews.lock().unwrap().insert(
        (
            id.to_string(),
            body["context"].as_str().unwrap().to_string(),
        ),
        body["selection"].clone(),
    );
    (
        StatusCode::OK,
        Json(
            json!({"id":id,"context":body["context"],"reviewed_revision":body["reviewed_revision"]}),
        ),
    )
}
async fn disconnect(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let id = body["id"].as_str().unwrap();
    if !authenticated(&state, &headers, id) {
        return denied();
    }
    state.sessions.lock().unwrap().remove(id);
    (StatusCode::OK, Json(json!({"id":id,"disconnected":true})))
}

async fn prepare(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authenticated(&state, &headers, body["id"].as_str().unwrap()) {
        return denied();
    }
    state.prepares.lock().unwrap().push(body);
    if state.block_prepare.load(Ordering::SeqCst) {
        state.prepare_started.notify_one();
        state.prepare_gate.acquire().await.unwrap().forget();
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"error":"review_required","current_revision":state.revision.load(Ordering::SeqCst).to_string()}),
            ),
        );
    }
    (
        StatusCode::OK,
        Json(state.prepare_response.lock().unwrap().clone()),
    )
}

async fn close_turn(
    State(state): State<Arc<ServerState>>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authenticated(&state, &headers, body["id"].as_str().unwrap()) {
        return denied();
    }
    let mut observed = body.clone();
    observed["path"] = json!(uri.path());
    state.closes.lock().unwrap().push(observed);
    if state.block_close.load(Ordering::SeqCst) {
        state.close_started.notify_one();
        state.close_gate.acquire().await.unwrap().forget();
    }
    state.closes_completed.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        Json(
            json!({"id":body["id"],"context":body["context"],"turn_id":body["turn_id"],"closed":true}),
        ),
    )
}

async fn gateway(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    state.gateway_requests.lock().unwrap().push((
        body,
        bearer(&headers).into(),
        headers.contains_key("x-direct-secret"),
    ));
    if state.reject_gateway_review.load(Ordering::SeqCst) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"review_required"})),
        )
            .into_response();
    }
    (
        [("content-type", "text/event-stream")],
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hub response\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    ).into_response()
}

async fn responses_gateway(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    state.gateway_requests.lock().unwrap().push((
        body,
        bearer(&headers).into(),
        headers.contains_key("x-direct-secret"),
    ));
    (
        [("content-type", "text/event-stream")],
        state.responses_stream.lock().unwrap().clone(),
    )
        .into_response()
}

impl Server {
    async fn start(interval: u64) -> Self {
        let state = Arc::new(ServerState {
            supports_delegated: AtomicBool::new(true),
            prepare_response: std::sync::Mutex::new(
                json!({"state":"waiting","reason":"busy","retry_after_ms":100}),
            ),
            prepares: Default::default(),
            block_prepare: AtomicBool::new(false),
            prepare_started: Notify::new(),
            prepare_gate: Semaphore::new(0),
            closes: Default::default(),
            block_close: AtomicBool::new(false),
            close_started: Notify::new(),
            close_gate: Semaphore::new(0),
            closes_completed: AtomicUsize::new(0),
            gateway_requests: Default::default(),
            responses_stream: Default::default(),
            reject_gateway_review: AtomicBool::new(false),
            hub_id: std::sync::Mutex::new("hub-test".into()),
            revision: AtomicU64::new(1),
            registrations: AtomicUsize::new(0),
            heartbeats: AtomicUsize::new(0),
            activities: Default::default(),
            heartbeat_turns: Default::default(),
            catalogs: AtomicUsize::new(0),
            block_catalog: AtomicBool::new(false),
            catalog_started: Notify::new(),
            catalog_gate: Semaphore::new(0),
            sessions: Default::default(),
            reviews: Default::default(),
            reject_review: AtomicBool::new(false),
            reject_heartbeat: AtomicBool::new(false),
            block_registration: AtomicBool::new(false),
            registration_started: Notify::new(),
            registration_gate: Semaphore::new(0),
            block_main_review: AtomicBool::new(false),
            review_started: Notify::new(),
            review_gate: Semaphore::new(0),
            block_heartbeat: AtomicBool::new(false),
            heartbeat_started: Notify::new(),
            heartbeat_gate: Semaphore::new(0),
            heartbeat_interval_ms: interval,
        });
        let router = Router::new()
            .route("/v1/clients/register", post(register))
            .route("/v1/catalog", get(catalog))
            .route("/v1/clients/heartbeat", post(heartbeat))
            .route("/v1/clients/review", post(review))
            .route("/v1/clients/disconnect", post(disconnect))
            .route("/v1/requests/prepare", post(prepare))
            .route("/v1/turns/finish", post(close_turn))
            .route("/v1/turns/cancel", post(close_turn))
            .route("/r/permit-1/v1/chat/completions", post(gateway))
            .route("/r/permit-1/v1/responses", post(responses_gateway))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
}

fn store(temp: &tempfile::TempDir) -> HubSettingsStore {
    HubSettingsStore::new(
        camino::Utf8PathBuf::from_path_buf(temp.path().join("hub-settings.json")).unwrap(),
    )
}
fn selection(model: &str) -> HubSelection {
    HubSelection {
        allowed_model_ids: [model.to_string()].into(),
        preferred_model_id: model.into(),
        required_capabilities: ["chat".to_string()].into(),
        wait_policy: crate::hub::HubWaitPolicy::WaitForPreferred,
        affinity_turns: 1,
    }
}
async fn connect_service(service: &HubConnection, server: &Server) -> HubConnectionProjection {
    let current = service.projection().await;
    service
        .connect(
            server.endpoint.clone(),
            BOOTSTRAP.into(),
            "My Desktop".into(),
            current.settings_revision,
            current.connection_generation,
        )
        .await
        .unwrap()
}
async fn save(
    service: &HubConnection,
    projection: &HubConnectionProjection,
    context: HubReviewContext,
    model: &str,
) -> Result<HubConnectionProjection, HubError> {
    service
        .save_review(
            context,
            selection(model),
            projection.hub_id.clone().unwrap(),
            projection.catalog.as_ref().unwrap().revision,
            projection.settings_revision.clone(),
            projection.connection_generation.clone(),
        )
        .await
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn strict_store_preserves_corruption_and_uses_canonical_revision_cas() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let initial = store.load().unwrap();
    assert_eq!(initial.revision, "0");
    let saved = store.save(&initial).unwrap();
    assert_eq!(saved.revision, "1");
    assert_eq!(store.save(&initial).unwrap_err(), HubError::SettingsChanged);
    let path = temp.path().join("hub-settings.json");
    for corrupt in [
        b"{broken".to_vec(),
        {
            let mut value = serde_json::to_value(&saved).unwrap();
            value["revision"] = json!("01");
            serde_json::to_vec(&value).unwrap()
        },
        {
            let mut value = serde_json::to_value(&saved).unwrap();
            value["token"] = json!("must-not-be-accepted");
            serde_json::to_vec(&value).unwrap()
        },
    ] {
        std::fs::write(&path, &corrupt).unwrap();
        assert_eq!(store.load().unwrap_err(), HubError::SettingsInvalid);
        assert_eq!(store.save(&saved).unwrap_err(), HubError::SettingsInvalid);
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }
}

#[test]
fn schema_one_reads_as_direct_and_schema_two_rejects_duplicate_or_missing_modes() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let path = temp.path().join("hub-settings.json");
    let legacy = r#"{"schema_version":1,"revision":"4","endpoint":"","label":"Desktop","hub_id":null,"main_review":null,"side_chat_review":null}"#;
    std::fs::write(&path, legacy).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.schema_version, 3);
    assert_eq!(loaded.main_mode, HubRouteMode::Direct);
    assert_eq!(loaded.side_chat_mode, HubRouteMode::Direct);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);
    let saved = store.save(&loaded).unwrap();
    assert_eq!(saved.revision, "5");
    for value in [
        legacy.replace("\"schema_version\":1", "\"schema_version\":2"),
        legacy.replace(
            "\"revision\":\"4\"",
            "\"revision\":\"3\",\"revision\":\"4\"",
        ),
        legacy.replace(
            "\"schema_version\":1",
            "\"schema_version\":1,\"main_mode\":\"direct\"",
        ),
    ] {
        std::fs::write(&path, value).unwrap();
        assert_eq!(store.load().unwrap_err(), HubError::SettingsInvalid);
    }
}

async fn enable(
    service: &HubConnection,
    current: HubConnectionProjection,
    context: HubReviewContext,
) -> HubConnectionProjection {
    service
        .set_route_mode(
            context,
            HubRouteMode::Hub,
            current.settings_revision,
            current.connection_generation,
        )
        .await
        .unwrap()
}

fn route_request() -> crate::llm::ChatRequest {
    use crate::config::{
        ProviderDeadlines, ProviderProfile, ProviderReasoningCapability, ProviderTarget,
    };
    use crate::llm::{ChatRequest, ModelCapabilities, ModelMessage, ModelProfile};
    let provider = ProviderTarget::new(
        "http://127.0.0.1:9/v1",
        "direct-must-not-run",
        ProviderProfile::OpenAiResponses,
        ProviderDeadlines {
            request_timeout_ms: 2000,
            connect_timeout_ms: 1000,
            max_connect_retries: 2,
        },
    )
    .unwrap();
    let mut request = ChatRequest::new(
        provider,
        ModelProfile {
            name: "direct-must-not-run".into(),
            context_window: 8192,
            max_output_tokens: 1024,
            provider_profile: ProviderProfile::OpenAiResponses,
            capabilities: ModelCapabilities {
                supports_tools: false,
                supports_reasoning: false,
                supports_images: false,
            },
        },
        "Keep the user's system instruction".into(),
        vec![ModelMessage::User {
            content: "question".into(),
        }],
        Vec::new(),
        None,
        ProviderReasoningCapability::Unsupported,
        [("x-direct-secret".into(), "private".into())].into(),
    );
    request.replace_api_key(Some("direct-private-key".into()));
    request
}

#[derive(Default)]
struct Output {
    text: String,
    phases: Vec<crate::llm::ProviderPhaseEvent>,
}
impl crate::llm::LlmEventSink for Output {
    fn push(&mut self, event: crate::llm::LlmEvent) -> Result<(), crate::error::LlmError> {
        if let crate::llm::LlmEvent::TextDelta(text) = event {
            self.text.push_str(&text);
        }
        Ok(())
    }
    fn provider_phase(
        &mut self,
        event: crate::llm::ProviderPhaseEvent,
    ) -> Result<(), crate::error::LlmError> {
        self.phases.push(event);
        Ok(())
    }
}

fn grant(server: &Server, model: &str) -> Value {
    json!({"state":"ready","lease_id":"lease-1","permit_id":"permit-1","logical_model_id":model,
        "gateway_base_url":format!("{}/r/permit-1/v1",server.endpoint),"request_token":"request-token-01234567890123456789",
        "expires_at_ms":"9999999999999","provider_profile":"openai_compatible_chat","provider_api_mode":"chat_completions"})
}

#[tokio::test]
async fn cancelling_a_slow_finish_releases_exact_local_scope_and_keeps_the_wire_close_owned() {
    for delegated in [false, true] {
        let server = Server::start(60_000).await;
        let temp = tempfile::tempdir().unwrap();
        let service = HubConnection::new(store(&temp));
        let connected = connect_service(&service, &server).await;
        let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
            .await
            .unwrap();
        enable(&service, reviewed, HubReviewContext::Main).await;
        let parent = service
            .begin_turn(HubReviewContext::Main, CancellationToken::new())
            .unwrap()
            .unwrap();
        let child = parent.fork_delegated(CancellationToken::new()).unwrap();
        let sibling = parent.fork_delegated(CancellationToken::new()).unwrap();
        let finishing = if delegated {
            child.clone()
        } else {
            parent.clone()
        };
        let guard = finishing.execution_guard();
        server.state.block_close.store(true, Ordering::SeqCst);
        let waiter = tokio::spawn(async move {
            guard.finish().await;
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server.state.close_started.notified(),
        )
        .await
        .unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        {
            let state = service.inner.state.lock().unwrap();
            assert_eq!(state.delegated_active.len(), if delegated { 1 } else { 2 });
            assert_eq!(
                state.active[HubReviewContext::Main.index()].is_some(),
                delegated
            );
        }
        let next = if !delegated {
            Some(
                service
                    .begin_turn(HubReviewContext::Main, CancellationToken::new())
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        server.state.block_close.store(false, Ordering::SeqCst);
        server.state.close_gate.add_permits(1);
        until(|| server.state.closes_completed.load(Ordering::SeqCst) == 1).await;
        assert_eq!(
            server.state.closes.lock().unwrap().len(),
            1,
            "close has one wire owner despite caller abort"
        );
        assert!(
            service.projection_now().active_main.is_some(),
            "delayed close cannot clear another root"
        );
        if let Some(next) = next {
            next.finish().await;
        }
        parent.finish().await;
        child.finish().await;
        sibling.finish().await;
        assert!(
            service
                .inner
                .state
                .lock()
                .unwrap()
                .delegated_active
                .is_empty()
        );
        service.shutdown().await;
    }
}

#[tokio::test]
async fn delegated_routes_survive_parent_finish_with_independent_heartbeat_permits_and_cancel() {
    let server = Server::start(100).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    *server.state.prepare_response.lock().unwrap() = grant(&server, "deep");
    let parent_cancel = CancellationToken::new();
    let parent = service
        .begin_turn(HubReviewContext::Main, parent_cancel.clone())
        .unwrap()
        .unwrap();
    let child_cancel = CancellationToken::new();
    let child = parent.fork_delegated(child_cancel.clone()).unwrap();
    let child_guard = child.execution_guard();
    parent.finish().await;
    assert!(service.projection_now().active_main.is_none());
    let next = service
        .begin_turn(HubReviewContext::Main, CancellationToken::new())
        .unwrap()
        .unwrap();
    let sibling = parent.fork_delegated(CancellationToken::new()).unwrap();
    let sibling_guard = sibling.execution_guard();
    let mut child_output = Output::default();
    let mut sibling_output = Output::default();
    let child_client = child.client();
    let sibling_client = sibling.client();
    let (child_result, sibling_result) = tokio::join!(
        child_client.stream_chat(route_request(), child_cancel.clone(), &mut child_output),
        sibling_client.stream_chat(
            route_request(),
            CancellationToken::new(),
            &mut sibling_output
        ),
    );
    child_result.unwrap();
    sibling_result.unwrap();
    assert_eq!(child_output.text, "Hub response");
    assert_eq!(sibling_output.text, "Hub response");
    let requests = server.state.prepares.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_ne!(requests[0]["turn_id"], requests[1]["turn_id"]);
    assert_ne!(requests[0]["request_id"], requests[1]["request_id"]);
    for request in &requests {
        assert_eq!(request["purpose"], "delegated");
        assert_eq!(request["context"], "main");
        assert_eq!(request["delegated_selection"]["preferred_model_id"], "deep");
    }
    until(|| {
        server
            .state
            .heartbeat_turns
            .lock()
            .unwrap()
            .last()
            .is_some_and(|value| {
                value.as_array().is_some_and(|turns| {
                    turns
                        .iter()
                        .filter(|turn| turn["delegated"] == true)
                        .count()
                        == 2
                })
            })
    })
    .await;
    child_cancel.cancel();
    child_guard.finish().await;
    assert!(!parent_cancel.is_cancelled());
    assert!(
        service.projection_now().active_main.is_some(),
        "child completion does not clear the next root"
    );
    sibling
        .client()
        .stream_chat(
            route_request(),
            CancellationToken::new(),
            &mut Output::default(),
        )
        .await
        .unwrap();
    let child_close = server.state.closes.lock().unwrap().last().unwrap().clone();
    assert_eq!(child_close["delegated"], true);
    assert_eq!(child_close["path"], "/v1/turns/cancel");
    next.finish().await;
    sibling_guard.finish().await;
    until(|| server.state.heartbeat_turns.lock().unwrap().last() == Some(&json!([]))).await;
    assert_eq!(server.state.registrations.load(Ordering::SeqCst), 1);
    service.shutdown().await;
}

#[tokio::test]
async fn delegated_routes_reject_legacy_hubs_and_changed_review_and_abort_on_disconnect() {
    let server = Server::start(60_000).await;
    server
        .state
        .supports_delegated
        .store(false, Ordering::SeqCst);
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    let parent = service
        .begin_turn(HubReviewContext::Main, CancellationToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        parent.fork_delegated(CancellationToken::new()).unwrap_err(),
        HubError::DelegatedExecutionUnsupported
    );
    parent.finish().await;
    server
        .state
        .supports_delegated
        .store(true, Ordering::SeqCst);
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    let parent = service
        .begin_turn(HubReviewContext::Main, CancellationToken::new())
        .unwrap()
        .unwrap();
    parent.finish().await;
    let changed = save(
        &service,
        &service.projection_now(),
        HubReviewContext::Main,
        "fast",
    )
    .await
    .unwrap();
    assert_eq!(
        parent.fork_delegated(CancellationToken::new()).unwrap_err(),
        HubError::ReviewRequired
    );
    let reviewed = save(&service, &changed, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    let child = parent.fork_delegated(CancellationToken::new()).unwrap();
    let guard = child.execution_guard();
    *server.state.prepare_response.lock().unwrap() =
        json!({"state":"waiting","reason":"busy","retry_after_ms":100});
    let child_client = child.client();
    let mut output = Output::default();
    let operation =
        child_client.stream_chat(route_request(), CancellationToken::new(), &mut output);
    let disconnect = async {
        until(|| !server.state.prepares.lock().unwrap().is_empty()).await;
        service.shutdown().await;
    };
    let (result, _) = tokio::join!(operation, disconnect);
    assert!(result.is_err());
    drop(guard);
    assert!(
        service
            .inner
            .state
            .lock()
            .unwrap()
            .delegated_active
            .is_empty()
    );
    assert!(server.state.gateway_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn heartbeat_reports_current_main_and_side_owners_and_drops_cancelled_or_finished_turns() {
    let server = Server::start(100).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let side = save(&service, &main, HubReviewContext::SideChat, "deep")
        .await
        .unwrap();
    let enabled = enable(&service, side, HubReviewContext::Main).await;
    enable(&service, enabled, HubReviewContext::SideChat).await;
    let main_cancel = CancellationToken::new();
    let main = service
        .begin_turn(HubReviewContext::Main, main_cancel.clone())
        .unwrap()
        .unwrap();
    let side = service
        .begin_turn(HubReviewContext::SideChat, CancellationToken::new())
        .unwrap()
        .unwrap();
    until(|| {
        server
            .state
            .heartbeat_turns
            .lock()
            .unwrap()
            .last()
            .is_some_and(|value| value.as_array().is_some_and(|turns| turns.len() == 2))
    })
    .await;
    let both = server
        .state
        .heartbeat_turns
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(both[0]["context"], "main");
    assert_eq!(both[1]["context"], "side_chat");
    assert_ne!(both[0]["turn_id"], both[1]["turn_id"]);
    main_cancel.cancel();
    until(|| {
        server
            .state
            .heartbeat_turns
            .lock()
            .unwrap()
            .last()
            .is_some_and(|value| value == &json!([both[1].clone()]))
    })
    .await;
    main.finish().await;
    side.finish().await;
    until(|| {
        server
            .state
            .heartbeat_turns
            .lock()
            .unwrap()
            .last()
            .is_some_and(|value| value == &json!([]))
    })
    .await;
    assert!(server.state.prepares.lock().unwrap().is_empty());
}

#[tokio::test]
async fn hub_turns_use_fresh_grants_without_direct_credentials_or_ephemeral_diagnostics() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let enabled = enable(&service, reviewed, HubReviewContext::Main).await;
    *server.state.prepare_response.lock().unwrap() = grant(&server, "deep");
    let cancel = CancellationToken::new();
    let route = service
        .begin_turn(HubReviewContext::Main, cancel.clone())
        .unwrap()
        .unwrap();
    assert_eq!(
        service
            .begin_turn(HubReviewContext::Main, cancel.clone())
            .unwrap_err(),
        HubError::RouteBusy
    );
    assert_eq!(
        service
            .set_route_mode(
                HubReviewContext::Main,
                HubRouteMode::Direct,
                enabled.settings_revision.clone(),
                enabled.connection_generation.clone()
            )
            .await
            .unwrap_err(),
        HubError::RouteBusy
    );
    let mut output = Output::default();
    let client = route.client();
    for _ in 0..2 {
        client
            .stream_chat(route_request(), cancel.clone(), &mut output)
            .await
            .unwrap();
    }
    assert_eq!(output.text, "Hub responseHub response");
    assert_eq!(route.metrics_model(), "deep");
    assert!(
        output
            .phases
            .iter()
            .all(|event| event.endpoint == enabled.endpoint)
    );
    let projection = service.projection_now();
    assert_eq!(
        projection.active_main.unwrap().logical_model_id.as_deref(),
        Some("deep")
    );
    let requests = server.state.prepares.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["turn_id"], requests[1]["turn_id"]);
    assert_ne!(requests[0]["request_id"], requests[1]["request_id"]);
    for (body, bearer, direct_header) in server.state.gateway_requests.lock().unwrap().iter() {
        assert_eq!(body["model"], "deep");
        assert!(!direct_header);
        assert_eq!(bearer, "Bearer request-token-01234567890123456789");
        assert!(
            body["messages"]
                .to_string()
                .contains("Keep the user's system instruction")
        );
    }
    let durable = std::fs::read_to_string(temp.path().join("hub-settings.json")).unwrap();
    for secret in [
        "request-token",
        "permit-1",
        "direct-private",
        "client-token",
    ] {
        assert!(!durable.contains(secret));
    }
    route.finish().await;
    assert!(service.projection_now().active_main.is_none());
    assert_eq!(server.state.closes.lock().unwrap().len(), 1);
    service.shutdown().await;
}

#[tokio::test]
async fn prompt_enhancer_uses_the_reviewed_main_route_without_a_direct_provider() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    *server.state.prepare_response.lock().unwrap() = grant(&server, "deep");
    let mut direct = crate::config::ResolvedConfig::default();
    direct.model.base_url.clear();
    direct.model.model.clear();
    direct.model.api_key_env = Some("UNCONFIGURED_DIRECT_CREDENTIAL".into());
    direct
        .model
        .extra_headers
        .insert("x-direct-secret".into(), "private".into());
    let cancel = CancellationToken::new();
    let route = service
        .begin_preparation(HubReviewContext::Main, cancel.clone())
        .unwrap()
        .unwrap();
    assert!(service.projection_now().active_side_chat.is_none());
    let config =
        crate::config::ResolvedTurnConfig::from_effective(&route.runtime_config(&direct)).unwrap();
    let result = crate::tui::prompt_enhance::enhance_prompt_with_client(
        &config,
        route.client().as_ref(),
        "clarify this request",
        cancel,
    )
    .await
    .unwrap();
    assert_eq!(result, "Hub response");
    assert!(direct.model.base_url.is_empty());
    assert_eq!(direct.model.extra_headers["x-direct-secret"], "private");
    let prepares = server.state.prepares.lock().unwrap().clone();
    assert_eq!(prepares.len(), 1);
    assert_eq!(prepares[0]["context"], "main");
    assert_eq!(prepares[0]["purpose"], "preparation");
    let requests = server.state.gateway_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0["model"], "deep");
    assert_eq!(requests[0].1, "Bearer request-token-01234567890123456789");
    assert!(!requests[0].2);
    route.finish().await;
    assert!(service.projection_now().active_main.is_none());
    assert_eq!(server.state.closes.lock().unwrap().len(), 1);

    let cancel = CancellationToken::new();
    let route = service
        .begin_preparation(HubReviewContext::Main, cancel.clone())
        .unwrap()
        .unwrap();
    server.state.revision.store(2, Ordering::SeqCst);
    service
        .refresh(service.projection_now().connection_generation)
        .await
        .unwrap();
    let error = crate::tui::prompt_enhance::enhance_prompt_with_client(
        &config,
        route.client().as_ref(),
        "second request",
        cancel,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains(HubError::ReviewRequired.code()));
    assert_eq!(server.state.prepares.lock().unwrap().len(), 1);
    route.finish().await;
    service.shutdown().await;
}

#[tokio::test]
async fn waiting_stop_closes_turn_and_disconnected_direct_switch_preserves_review() {
    let server = Server::start(100).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let both = save(&service, &main, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    let enabled = enable(&service, both, HubReviewContext::Main).await;
    enable(&service, enabled, HubReviewContext::SideChat).await;
    let preparation_cancel = CancellationToken::new();
    let route = service
        .begin_turn(HubReviewContext::Main, preparation_cancel.clone())
        .unwrap()
        .unwrap();
    // RunService replaces the preparation control with the admitted run owner.
    // An individual LLM request must not own that replacement.
    let cancel = CancellationToken::new();
    route.bind_run_control(cancel.clone());
    let main_turn_id = service.projection_now().active_main.unwrap().turn_id;
    let side_cancel = CancellationToken::new();
    let side = service
        .begin_turn(HubReviewContext::SideChat, side_cancel.clone())
        .unwrap()
        .unwrap();
    let side_turn_id = service.projection_now().active_side_chat.unwrap().turn_id;
    let client = route.client();
    let mut output = Output::default();
    let result = client.stream_chat(route_request(), cancel.child_token(), &mut output);
    let stop = async {
        until(|| {
            server
                .state
                .activities
                .lock()
                .unwrap()
                .iter()
                .any(|activity| activity == "waiting")
        })
        .await;
        cancel.cancel();
    };
    let (result, _) = tokio::join!(result, stop);
    assert!(result.is_err());
    assert!(server.state.gateway_requests.lock().unwrap().is_empty());
    assert!(!preparation_cancel.is_cancelled());
    assert!(!side_cancel.is_cancelled());
    route.finish().await;
    assert_eq!(
        service.projection_now().active_side_chat.unwrap().turn_id,
        side_turn_id
    );
    assert!(service.projection_now().active_main.is_none());
    {
        let closes = server.state.closes.lock().unwrap();
        assert_eq!(closes.len(), 1);
        assert!(closes.iter().any(|close| {
            close["context"] == "main"
                && close["turn_id"] == main_turn_id
                && close["path"] == "/v1/turns/cancel"
                && close.get("delegated").is_none()
        }));
    }
    side.finish().await;
    {
        let closes = server.state.closes.lock().unwrap();
        assert_eq!(closes.len(), 2);
        assert!(closes.iter().any(|close| {
            close["context"] == "side_chat"
                && close["turn_id"] == side_turn_id
                && close["path"] == "/v1/turns/finish"
                && close.get("delegated").is_none()
        }));
    }
    service.shutdown().await;
    let current = service.projection_now();
    assert!(
        service
            .begin_turn(HubReviewContext::Main, CancellationToken::new())
            .is_err()
    );
    let direct = service
        .set_route_mode(
            HubReviewContext::Main,
            HubRouteMode::Direct,
            current.settings_revision,
            current.connection_generation,
        )
        .await
        .unwrap();
    assert_eq!(
        direct
            .main_review
            .as_ref()
            .unwrap()
            .selection
            .preferred_model_id,
        "deep"
    );
    assert!(
        service
            .begin_turn(HubReviewContext::Main, CancellationToken::new())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn request_cancellation_does_not_replace_the_admitted_turn_control() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    let route = service
        .begin_turn(HubReviewContext::Main, CancellationToken::new())
        .unwrap()
        .unwrap();
    let admitted_cancel = CancellationToken::new();
    route.bind_run_control(admitted_cancel.clone());
    let turn_id = service.projection_now().active_main.unwrap().turn_id;
    let request_cancel = CancellationToken::new();
    let client = route.client();
    let mut output = Output::default();
    let request = client.stream_chat(route_request(), request_cancel.clone(), &mut output);
    let cancel_request = async {
        until(|| !server.state.prepares.lock().unwrap().is_empty()).await;
        request_cancel.cancel();
    };
    let (result, _) = tokio::join!(request, cancel_request);
    assert!(result.is_err());
    assert!(!admitted_cancel.is_cancelled());
    assert_eq!(
        service.projection_now().active_main.unwrap().turn_id,
        turn_id
    );
    assert!(server.state.gateway_requests.lock().unwrap().is_empty());
    assert!(server.state.closes.lock().unwrap().is_empty());

    // Closing immediately keeps the cancelled request as the most recent request.
    // It must not turn a live admitted owner into a cancelled scope.
    route.finish().await;
    assert!(service.projection_now().active_main.is_none());
    {
        let closes = server.state.closes.lock().unwrap();
        assert_eq!(closes.len(), 1);
        assert!(closes.iter().any(|close| {
            close["context"] == "main"
                && close["turn_id"] == turn_id
                && close["path"] == "/v1/turns/finish"
        }));
    }
    service.shutdown().await;
}

#[tokio::test]
async fn gateway_failures_and_invalid_grants_do_not_expose_a_transient_target() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable_endpoint =
        format!("http://{}/r/permit-1/v1", unavailable.local_addr().unwrap());
    drop(unavailable);
    let mut response = grant(&server, "deep");
    response["gateway_base_url"] = json!(unavailable_endpoint);
    *server.state.prepare_response.lock().unwrap() = response;
    let cancel = CancellationToken::new();
    let route = service
        .begin_turn(HubReviewContext::Main, cancel.clone())
        .unwrap()
        .unwrap();
    let mut output = Output::default();
    let error = route
        .client()
        .stream_chat(route_request(), cancel.clone(), &mut output)
        .await
        .unwrap_err();
    let evidence = format!(
        "{error:?}\n{error}\n{:?}\n{}",
        output.phases,
        serde_json::to_string(&service.projection_now()).unwrap()
    );
    for secret in [
        unavailable_endpoint.as_str(),
        "/r/permit-1",
        "request-token",
        "direct-private",
    ] {
        assert!(
            !evidence.contains(secret),
            "transient target escaped diagnostics"
        );
    }
    route.finish().await;
    for (field, bad) in [
        ("gateway_base_url", "http://192.0.2.1/r/permit-1/v1"),
        ("logical_model_id", "unreviewed"),
        ("provider_api_mode", "responses"),
        ("provider_profile", "openai_compatible_responses"),
        ("provider_profile", "lmstudio_responses"),
    ] {
        let mut response = grant(&server, "deep");
        response[field] = json!(bad);
        *server.state.prepare_response.lock().unwrap() = response;
        let route = service
            .begin_turn(HubReviewContext::Main, cancel.clone())
            .unwrap()
            .unwrap();
        assert!(
            route
                .client()
                .stream_chat(route_request(), cancel.clone(), &mut Output::default())
                .await
                .is_err()
        );
        route.finish().await;
    }
    assert!(server.state.gateway_requests.lock().unwrap().is_empty());
    service.shutdown().await;
}

#[tokio::test]
async fn hub_responses_uses_the_captured_adapter_and_never_replays_terminal_failures() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    let mut ready = grant(&server, "deep");
    ready["provider_profile"] = json!("openai_compatible_responses");
    ready["provider_api_mode"] = json!("responses");
    *server.state.prepare_response.lock().unwrap() = ready;
    for (index, status) in ["completed", "failed", "incomplete", "truncated"]
        .into_iter()
        .enumerate()
    {
        let terminal = json!({"type":format!("response.{status}"),"response":{
            "id":"response-hub", "status":status,
            "output":[{"id":"message-hub","type":"message","role":"assistant","status":"completed",
                "content":[{"type":"output_text","text":"Hub Responses result","annotations":[]}]}],
            "error":{"code":"server_error","message":"fixture failure"},
            "incomplete_details":{"reason":"max_output_tokens"}
        }});
        *server.state.responses_stream.lock().unwrap() = if status == "truncated" {
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"response-hub\",\"status\":\"in_progress\"}}\n\n".into()
        } else {
            format!("data: {terminal}\n\n")
        };
        let cancel = CancellationToken::new();
        let route = service
            .begin_turn(HubReviewContext::Main, cancel.clone())
            .unwrap()
            .unwrap();
        let mut output = Output::default();
        let result = route
            .client()
            .stream_chat(route_request(), cancel, &mut output)
            .await;
        assert_eq!(
            result.is_ok(),
            status == "completed",
            "{status}: {result:?}"
        );
        if status == "completed" {
            assert_eq!(output.text, "Hub Responses result");
        }
        route.finish().await;
        let records = server.state.gateway_requests.lock().unwrap();
        assert_eq!(records.len(), index + 1, "no automatic retry on {status}");
        let (body, bearer, direct_header) = records.last().unwrap();
        assert_eq!(body["model"], "deep");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body["input"].is_array());
        assert!(
            body["instructions"]
                .as_str()
                .unwrap()
                .contains("Keep the user's system instruction")
        );
        assert!(body.get("messages").is_none());
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(bearer, "Bearer request-token-01234567890123456789");
        assert!(!direct_header);
    }
    assert_eq!(server.state.prepares.lock().unwrap().len(), 4);
    service.shutdown().await;
}

#[tokio::test]
async fn runtime_policy_preserves_local_settings_and_refuses_catalog_drift_before_request() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    let cancel = CancellationToken::new();
    let route = service
        .begin_turn(HubReviewContext::Main, cancel.clone())
        .unwrap()
        .unwrap();
    let mut direct = crate::config::ResolvedConfig::default();
    direct.model.model = "direct-preserved".into();
    direct.model.api_key_env = Some("DIRECT_SECRET_ENV".into());
    direct
        .model
        .extra_headers
        .insert("x-private".into(), "secret".into());
    direct.model.system_prompt = "user's instruction".into();
    direct.model.context_window = 12345;
    direct.multi_agent.enabled = true;
    let runtime = route.runtime_config(&direct);
    assert_eq!(runtime.model.model, "deep");
    assert_eq!(runtime.model.system_prompt, direct.model.system_prompt);
    assert_eq!(runtime.model.context_window, direct.model.context_window);
    assert_eq!(
        runtime.permissions.access_mode,
        direct.permissions.access_mode
    );
    assert!(runtime.multi_agent.enabled);
    assert!(!runtime.model.supports_tools);
    assert!(runtime.model.api_key_env.is_none());
    assert!(runtime.model.extra_headers.is_empty());
    assert_eq!(direct.model.model, "direct-preserved");
    server.state.revision.store(2, Ordering::SeqCst);
    service
        .refresh(service.projection_now().connection_generation)
        .await
        .unwrap();
    let error = route
        .client()
        .stream_chat(route_request(), cancel, &mut Output::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains(HubError::ReviewRequired.code()));
    assert!(server.state.prepares.lock().unwrap().is_empty());
    route.finish().await;
    service.shutdown().await;
}

#[tokio::test]
async fn delayed_old_main_prepare_rejection_preserves_new_side_confirmation() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let both = save(&service, &main, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    enable(&service, both, HubReviewContext::Main).await;
    let cancel = CancellationToken::new();
    let route = service
        .begin_turn(HubReviewContext::Main, cancel.clone())
        .unwrap()
        .unwrap();
    server.state.block_prepare.store(true, Ordering::SeqCst);
    let client = route.client();
    let mut output = Output::default();
    let request = client.stream_chat(route_request(), cancel, &mut output);
    let review_newer = async {
        server.state.prepare_started.notified().await;
        server.state.revision.store(2, Ordering::SeqCst);
        let refreshed = service
            .refresh(service.projection_now().connection_generation)
            .await
            .unwrap();
        save(&service, &refreshed, HubReviewContext::SideChat, "fast")
            .await
            .unwrap();
        server.state.prepare_gate.add_permits(1);
    };
    let (result, _) = tokio::join!(request, review_newer);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains(HubError::ReviewRequired.code())
    );
    let projection = service.projection_now();
    assert_eq!(
        projection.main_confirmation,
        HubReviewConfirmation::ReviewRequired
    );
    assert_eq!(
        projection.side_chat_confirmation,
        HubReviewConfirmation::Confirmed
    );
    assert!(projection.error.is_none());
    route.finish().await;
    service.shutdown().await;
}

#[tokio::test]
async fn gateway_claim_revision_rejection_invalidates_confirmation_without_direct_fallback() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let reviewed = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    enable(&service, reviewed, HubReviewContext::Main).await;
    *server.state.prepare_response.lock().unwrap() = grant(&server, "deep");
    server
        .state
        .reject_gateway_review
        .store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let route = service
        .begin_turn(HubReviewContext::Main, cancel.clone())
        .unwrap()
        .unwrap();
    let error = route
        .client()
        .stream_chat(route_request(), cancel, &mut Output::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains(HubError::ReviewRequired.code()));
    assert_eq!(
        service.projection_now().main_confirmation,
        HubReviewConfirmation::ReviewRequired
    );
    assert_eq!(server.state.gateway_requests.lock().unwrap().len(), 1);
    route.finish().await;
    service.shutdown().await;
}

#[test]
fn simultaneous_store_writers_never_overwrite_a_newer_revision() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.save(&HubSettings::default())
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(error, HubError::SettingsBusy | HubError::SettingsChanged))
    );
    assert_eq!(store.load().unwrap().revision, "1");
}

#[tokio::test]
async fn catalog_review_baselines_follow_independent_durable_reviews_across_refresh_and_restart() {
    use crate::hub::HubCatalogComparisonStatus;
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let persisted = store(&temp);
    let service = HubConnection::new(persisted.clone());
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    assert_eq!(
        main.main_catalog_comparison.status,
        HubCatalogComparisonStatus::Compared
    );
    let first = persisted.load().unwrap();
    let main_baseline = first.main_catalog_baseline.clone();
    assert_eq!(
        main_baseline.as_ref().unwrap().revision,
        CatalogRevision::new(1).unwrap()
    );
    assert!(first.side_chat_catalog_baseline.is_none());

    server.state.revision.store(2, Ordering::SeqCst);
    let refreshed = service
        .refresh(main.connection_generation.clone())
        .await
        .unwrap();
    assert_eq!(
        refreshed.main_catalog_comparison.reviewed_revision,
        Some(CatalogRevision::new(1).unwrap())
    );
    assert_eq!(
        refreshed.main_catalog_comparison.current_revision,
        Some(CatalogRevision::new(2).unwrap())
    );
    assert_eq!(
        persisted.load().unwrap().main_catalog_baseline,
        main_baseline
    );
    let side = save(&service, &refreshed, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    let both = persisted.load().unwrap();
    assert_eq!(both.main_catalog_baseline, main_baseline);
    assert_eq!(
        both.side_chat_catalog_baseline.as_ref().unwrap().revision,
        CatalogRevision::new(2).unwrap()
    );
    assert_eq!(
        save(&service, &refreshed, HubReviewContext::Main, "fast")
            .await
            .unwrap_err(),
        HubError::SettingsChanged
    );
    assert_eq!(persisted.load().unwrap(), both);

    service.shutdown().await;
    let restarted = HubConnection::new(persisted.clone());
    assert_eq!(
        restarted.projection_now().main_catalog_comparison.status,
        HubCatalogComparisonStatus::CurrentUnavailable
    );
    let reconnected = connect_service(&restarted, &server).await;
    assert_eq!(
        reconnected.main_catalog_comparison.reviewed_revision,
        main.main_catalog_comparison.reviewed_revision
    );
    assert_eq!(
        reconnected.side_chat_catalog_comparison.reviewed_revision,
        side.side_chat_catalog_comparison.reviewed_revision
    );
    assert_eq!(persisted.load().unwrap(), both);
    let reviewed = save(&restarted, &reconnected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    assert_eq!(
        reviewed.main_catalog_comparison.reviewed_revision,
        Some(CatalogRevision::new(2).unwrap())
    );
    assert_eq!(
        persisted.load().unwrap().side_chat_catalog_baseline,
        both.side_chat_catalog_baseline
    );
    restarted.shutdown().await;
}

#[tokio::test]
async fn main_and_side_are_independent_and_restart_is_disconnected_without_secrets() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let service = HubConnection::new(store.clone());
    assert_eq!(
        service.projection().await.status,
        HubConnectionStatus::Disconnected
    );
    assert_eq!(server.state.registrations.load(Ordering::SeqCst), 0);
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let both = save(&service, &main, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    assert_eq!(both.main_confirmation, HubReviewConfirmation::Confirmed);
    assert_eq!(
        both.side_chat_confirmation,
        HubReviewConfirmation::Confirmed
    );
    assert_eq!(
        both.main_review
            .as_ref()
            .unwrap()
            .selection
            .preferred_model_id,
        "deep"
    );
    assert_eq!(server.state.reviews.lock().unwrap().len(), 2);
    let serialized = serde_json::to_string(&both).unwrap();
    let durable = std::fs::read_to_string(temp.path().join("hub-settings.json")).unwrap();
    for text in [serialized, durable, format!("{both:?}")] {
        for secret in [
            BOOTSTRAP,
            "client-token",
            "desktop-0",
            "client_token",
            "authorization",
        ] {
            assert!(!text.contains(secret), "secret leaked");
        }
    }
    service.shutdown().await;
    assert!(server.state.sessions.lock().unwrap().is_empty());
    let restarted = HubConnection::new(store);
    let restart = restarted.projection().await;
    assert_eq!(restart.status, HubConnectionStatus::Disconnected);
    assert_eq!(
        restart.main_confirmation,
        HubReviewConfirmation::Unconfirmed
    );
    assert_eq!(restart.main_review, both.main_review);
    assert_eq!(restart.side_chat_review, both.side_chat_review);
    assert!(restart.catalog.is_none());
    assert_eq!(server.state.registrations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn remote_review_failure_keeps_local_selection_unconfirmed_and_allows_explicit_retry() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    server.state.reject_review.store(true, Ordering::SeqCst);
    assert_eq!(
        save(&service, &connected, HubReviewContext::Main, "deep")
            .await
            .unwrap_err(),
        HubError::ReviewRequired
    );
    let failed = service.projection().await;
    assert_eq!(
        failed.main_confirmation,
        HubReviewConfirmation::ReviewRequired
    );
    assert!(failed.main_review.is_some());
    assert_ne!(failed.settings_revision, connected.settings_revision);
    assert_eq!(
        save(&service, &connected, HubReviewContext::Main, "fast")
            .await
            .unwrap_err(),
        HubError::SettingsChanged
    );
    server.state.reject_review.store(false, Ordering::SeqCst);
    assert_eq!(
        save(&service, &failed, HubReviewContext::Main, "deep")
            .await
            .unwrap()
            .main_confirmation,
        HubReviewConfirmation::Confirmed
    );
    service.shutdown().await;
}

#[tokio::test]
async fn abandoned_connect_is_revoked_and_cannot_resurrect_after_disconnect() {
    let server = Server::start(60_000).await;
    server
        .state
        .block_registration
        .store(true, Ordering::SeqCst);
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let worker = service.clone();
    let endpoint = server.endpoint.clone();
    let task = tokio::spawn(async move {
        worker
            .connect(
                endpoint,
                BOOTSTRAP.into(),
                "Desktop".into(),
                "0".into(),
                "0".into(),
            )
            .await
    });
    server.state.registration_started.notified().await;
    let connecting = service.projection().await;
    assert_eq!(connecting.status, HubConnectionStatus::Connecting);
    let disconnected = service
        .disconnect(connecting.connection_generation)
        .await
        .unwrap();
    server.state.registration_gate.add_permits(1);
    assert_eq!(
        task.await.unwrap().unwrap_err(),
        HubError::ConnectionChanged
    );
    until(|| server.state.sessions.lock().unwrap().is_empty()).await;
    let final_state = service.projection().await;
    assert_eq!(final_state.status, HubConnectionStatus::Disconnected);
    assert_eq!(
        final_state.connection_generation,
        disconnected.connection_generation
    );
    assert_eq!(final_state.settings_revision, "0");
    assert_eq!(server.state.catalogs.load(Ordering::SeqCst), 0);
    assert!(!temp.path().join("hub-settings.json").exists());
}

#[tokio::test]
async fn side_save_during_main_acknowledgement_preserves_both_independent_confirmations() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    server.state.block_main_review.store(true, Ordering::SeqCst);
    let worker = service.clone();
    let task =
        tokio::spawn(
            async move { save(&worker, &connected, HubReviewContext::Main, "deep").await },
        );
    server.state.review_started.notified().await;
    let pending_main = service.projection().await;
    assert_eq!(
        pending_main.main_confirmation,
        HubReviewConfirmation::Unconfirmed
    );
    let side = save(&service, &pending_main, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    assert_eq!(
        side.side_chat_confirmation,
        HubReviewConfirmation::Confirmed
    );
    server.state.review_gate.add_permits(1);
    let completed = task.await.unwrap().unwrap();
    assert_eq!(completed.settings_revision, side.settings_revision);
    assert_eq!(
        completed.main_confirmation,
        HubReviewConfirmation::Confirmed
    );
    assert_eq!(
        completed.side_chat_confirmation,
        HubReviewConfirmation::Confirmed
    );
    assert_eq!(server.state.reviews.lock().unwrap().len(), 2);
    service.shutdown().await;
}

#[tokio::test]
async fn stale_review_targets_never_persist_or_reach_the_remote_review_api() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    for (hub_id, revision, generation, expected) in [
        (
            "wrong-hub",
            "1",
            connected.connection_generation.as_str(),
            HubError::DifferentHub,
        ),
        (
            "hub-test",
            "2",
            connected.connection_generation.as_str(),
            HubError::ReviewRequired,
        ),
        ("hub-test", "1", "0", HubError::ConnectionChanged),
    ] {
        let revision = serde_json::from_value(json!(revision)).unwrap();
        assert_eq!(
            service
                .save_review(
                    HubReviewContext::Main,
                    selection("deep"),
                    hub_id.into(),
                    revision,
                    connected.settings_revision.clone(),
                    generation.into()
                )
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(
            service.projection().await.settings_revision,
            connected.settings_revision
        );
        assert!(store(&temp).load().unwrap().main_review.is_none());
    }
    assert!(server.state.reviews.lock().unwrap().is_empty());
    service.shutdown().await;
}

#[tokio::test]
async fn heartbeat_requires_review_while_the_new_catalog_response_is_pending() {
    let server = Server::start(100).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    assert_eq!(main.main_confirmation, HubReviewConfirmation::Confirmed);

    server.state.block_catalog.store(true, Ordering::SeqCst);
    server.state.revision.store(2, Ordering::SeqCst);
    tokio::time::timeout(
        Duration::from_secs(2),
        server.state.catalog_started.notified(),
    )
    .await
    .unwrap();

    // The old catalog and local CAS remain unchanged while HTTP is pending. The
    // projection must still distinguish this invalidation from a local save's pre-ack poll.
    let pending = service.projection().await;
    assert_eq!(pending.status, HubConnectionStatus::Connected);
    assert_eq!(pending.settings_revision, main.settings_revision);
    assert_eq!(pending.connection_generation, main.connection_generation);
    assert_eq!(
        pending.catalog.as_ref().unwrap().revision,
        CatalogRevision::new(1).unwrap()
    );
    assert_eq!(pending.error, Some("catalog_changed"));
    assert_eq!(
        pending.main_confirmation,
        HubReviewConfirmation::ReviewRequired
    );
    assert_eq!(
        pending.side_chat_confirmation,
        HubReviewConfirmation::Unconfirmed
    );
    assert!(!pending.can_enable_main_hub);
    assert!(!pending.can_enable_side_chat_hub);

    server.state.block_catalog.store(false, Ordering::SeqCst);
    server.state.catalog_gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if service
                .projection()
                .await
                .catalog
                .as_ref()
                .unwrap()
                .revision
                == CatalogRevision::new(2).unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let refreshed = service.projection().await;
    assert_eq!(refreshed.error, None);
    assert_eq!(refreshed.settings_revision, main.settings_revision);
    assert_eq!(
        refreshed.main_confirmation,
        HubReviewConfirmation::ReviewRequired
    );
    assert!(!refreshed.can_enable_main_hub);
    let reviewed = save(&service, &refreshed, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    assert_eq!(reviewed.main_confirmation, HubReviewConfirmation::Confirmed);
    assert!(reviewed.can_enable_main_hub);
    service.shutdown().await;
}

#[tokio::test]
async fn delayed_heartbeat_does_not_erase_newer_explicit_review_acknowledgements() {
    let server = Server::start(100).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    server.state.block_heartbeat.store(true, Ordering::SeqCst);
    server.state.revision.store(2, Ordering::SeqCst);
    server.state.heartbeat_started.notified().await;
    let refreshed = service
        .refresh(connected.connection_generation)
        .await
        .unwrap();
    let main = save(&service, &refreshed, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let both = save(&service, &main, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    assert_eq!(both.main_confirmation, HubReviewConfirmation::Confirmed);
    server.state.block_heartbeat.store(false, Ordering::SeqCst);
    server.state.heartbeat_gate.add_permits(1);
    until(|| server.state.heartbeats.load(Ordering::SeqCst) >= 2).await;
    let current = service.projection().await;
    assert_eq!(current.main_confirmation, HubReviewConfirmation::Confirmed);
    assert_eq!(
        current.side_chat_confirmation,
        HubReviewConfirmation::Confirmed
    );
    assert_eq!(server.state.catalogs.load(Ordering::SeqCst), 2);
    service.shutdown().await;
}

#[tokio::test]
async fn remote_revision_rejection_immediately_invalidates_both_old_confirmations() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let both = save(&service, &main, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    server.state.revision.store(2, Ordering::SeqCst);
    server.state.reject_review.store(true, Ordering::SeqCst);
    assert_eq!(
        save(&service, &both, HubReviewContext::Main, "deep")
            .await
            .unwrap_err(),
        HubError::ReviewRequired
    );
    let current = service.projection().await;
    assert_eq!(
        current.main_confirmation,
        HubReviewConfirmation::ReviewRequired
    );
    assert_eq!(
        current.side_chat_confirmation,
        HubReviewConfirmation::ReviewRequired
    );
    assert_eq!(current.error, Some("catalog_changed"));
    service.shutdown().await;
}

#[tokio::test]
async fn heartbeat_detects_revision_changes_and_auth_loss_without_reconnect() {
    let server = Server::start(100).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    server.state.revision.store(2, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = service.projection().await;
            if current.main_confirmation == HubReviewConfirmation::ReviewRequired {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    server.state.reject_heartbeat.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = service.projection().await;
            if current.status == HubConnectionStatus::Stale {
                assert_eq!(current.error, Some("unauthorized"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let count = server.state.heartbeats.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(server.state.heartbeats.load(Ordering::SeqCst), count);
    assert_eq!(server.state.registrations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auth_failure_refusal_and_corrupt_startup_are_safe_and_do_not_overwrite_settings() {
    let server = Server::start(60_000).await;
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    assert_eq!(
        service
            .connect(
                server.endpoint.clone(),
                "wrong-token-012345678901234567890".into(),
                "Desktop".into(),
                "0".into(),
                "0".into()
            )
            .await
            .unwrap_err(),
        HubError::Unauthorized
    );
    let current = service.projection().await;
    assert_eq!(current.error, Some("unauthorized"));
    assert!(!temp.path().join("hub-settings.json").exists());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        service
            .connect(
                endpoint,
                BOOTSTRAP.into(),
                "Desktop".into(),
                "0".into(),
                current.connection_generation
            )
            .await
            .unwrap_err(),
        HubError::Unavailable
    );
    std::fs::write(temp.path().join("hub-settings.json"), "broken").unwrap();
    let corrupt = HubConnection::new(store(&temp));
    assert_eq!(corrupt.projection().await.error, Some("settings_invalid"));
    assert_eq!(
        corrupt
            .connect(
                server.endpoint.clone(),
                BOOTSTRAP.into(),
                "Desktop".into(),
                "0".into(),
                "0".into()
            )
            .await
            .unwrap_err(),
        HubError::SettingsInvalid
    );
    assert_eq!(server.state.registrations.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("hub-settings.json")).unwrap(),
        "broken"
    );
}

#[tokio::test]
async fn endpoint_switch_clears_both_reviews_and_same_endpoint_replacement_is_rejected() {
    let server = Server::start(60_000).await;
    let other = Server::start(60_000).await;
    *other.state.hub_id.lock().unwrap() = "hub-other".into();
    let temp = tempfile::tempdir().unwrap();
    let service = HubConnection::new(store(&temp));
    let connected = connect_service(&service, &server).await;
    let main = save(&service, &connected, HubReviewContext::Main, "deep")
        .await
        .unwrap();
    let both = save(&service, &main, HubReviewContext::SideChat, "fast")
        .await
        .unwrap();
    *server.state.hub_id.lock().unwrap() = "replacement-hub".into();
    assert_eq!(
        service
            .connect(
                server.endpoint.clone(),
                BOOTSTRAP.into(),
                "Desktop".into(),
                both.settings_revision,
                both.connection_generation
            )
            .await
            .unwrap_err(),
        HubError::DifferentHub
    );
    assert!(service.projection().await.main_review.is_some());
    let switched = connect_service(&service, &other).await;
    assert_eq!(switched.hub_id.as_deref(), Some("hub-other"));
    assert!(switched.main_review.is_none());
    assert!(switched.side_chat_review.is_none());
    assert_eq!(
        switched.main_confirmation,
        HubReviewConfirmation::Unconfirmed
    );
    service.shutdown().await;
}
