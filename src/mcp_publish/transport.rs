//! Authenticated, JSON-response Streamable HTTP for MCP protocol 2025-11-25.
//! This intentionally implements the session-based revision, not the 2026 stateless protocol.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
#[cfg(test)]
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, Sleep};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::dispatch::{DeviceRequestAuthenticator, PublishCallError, PublishToolDispatcher};
use crate::device_network::{GrantClaims, VerifiedGrant};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_BODY: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_HTTP_REQUESTS: usize = 32;
const MAX_CONNECTIONS: usize = 64;
const MAX_SESSIONS: usize = 32;
const MAX_REQUEST_IDS: usize = 1024;
const MAX_RECENT_CALLS: usize = 64;
const SESSION_IDLE: Duration = Duration::from_secs(300);
const BODY_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECTION_IDLE: Duration = Duration::from_secs(90);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize)]
pub struct PublishTransportSnapshot {
    pub accepting: bool,
    pub live: bool,
    pub active_calls: usize,
    pub sessions: usize,
    pub recent_calls: Vec<PublishCallRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishCallRecord {
    /// Server-minted audit identity, never a caller-supplied request ID.
    pub id: String,
    pub tool: String,
    pub status: &'static str,
}

struct CallRecord {
    view: PublishCallRecord,
    cancel: CancellationToken,
    slot: Option<OwnedSemaphorePermit>,
}

struct Session {
    authority: Option<GrantClaims>,
    initialized: bool,
    touched: Instant,
    used_ids: HashSet<String>,
    active: HashMap<String, CancellationToken>,
    cancel: CancellationToken,
}

struct Shared {
    address: SocketAddr,
    scheme: &'static str,
    authentication: TransportAuthentication,
    dispatcher: Arc<dyn PublishToolDispatcher>,
    cancel: CancellationToken,
    sessions: Mutex<HashMap<String, Session>>,
    http_requests: Arc<Semaphore>,
    calls: Arc<Semaphore>,
    records: Mutex<VecDeque<CallRecord>>,
    workers: TaskTracker,
}

enum TransportAuthentication {
    Manual([u8; 32]),
    Managed(Arc<dyn DeviceRequestAuthenticator>),
}

impl Shared {
    fn cancel(&self) {
        // This lock also encloses worker admission, so closing the tracker cannot
        // race a late admitted worker absent from the drain observation.
        let mut sessions = self.sessions.lock().expect("MCP sessions poisoned");
        self.cancel.cancel();
        for session in sessions.values() {
            session.cancel.cancel();
        }
        sessions.clear();
        self.workers.close();
    }
}

/// The profile lifecycle retains this handle until `stop` reports complete drain.
/// No plaintext credential is stored by the transport.
pub struct PublishHttpServer {
    shared: Arc<Shared>,
    task: Option<JoinHandle<io::Result<()>>>,
    observer: PublishTransportObserver,
}

#[derive(Clone)]
pub struct PublishTransportObserver {
    shared: Arc<Shared>,
    task: tokio::task::AbortHandle,
}

impl PublishTransportObserver {
    pub fn snapshot(&self) -> PublishTransportSnapshot {
        let mut sessions = self.shared.sessions.lock().expect("MCP sessions poisoned");
        expire_sessions(&mut sessions);
        let live = !self.task.is_finished();
        let records = self.shared.records.lock().expect("MCP records poisoned");
        PublishTransportSnapshot {
            accepting: !self.shared.cancel.is_cancelled() && live,
            live,
            active_calls: records
                .iter()
                .filter(|record| record.slot.is_some())
                .count(),
            sessions: sessions.len(),
            recent_calls: records
                .iter()
                .map(|record| {
                    let mut view = record.view.clone();
                    if record.slot.is_some() && record.cancel.is_cancelled() {
                        view.status = "cancelling";
                    }
                    view
                })
                .collect(),
        }
    }
}

impl PublishHttpServer {
    pub async fn start(
        bind: SocketAddr,
        token_sha256: [u8; 32],
        dispatcher: Arc<dyn PublishToolDispatcher>,
        max_concurrent_calls: u16,
    ) -> io::Result<Self> {
        Self::start_with_tls(bind, token_sha256, dispatcher, max_concurrent_calls, None).await
    }

    pub async fn start_with_tls(
        bind: SocketAddr,
        token_sha256: [u8; 32],
        dispatcher: Arc<dyn PublishToolDispatcher>,
        max_concurrent_calls: u16,
        tls: Option<&super::PublishTls>,
    ) -> io::Result<Self> {
        let acceptor = tls.map(super::tls::load_acceptor).transpose()?;
        Self::start_authenticated(
            bind,
            dispatcher,
            max_concurrent_calls,
            acceptor,
            TransportAuthentication::Manual(token_sha256),
        )
        .await
    }

    pub async fn start_managed(
        bind: SocketAddr,
        dispatcher: Arc<dyn PublishToolDispatcher>,
        max_concurrent_calls: u16,
        tls_acceptor: tokio_rustls::TlsAcceptor,
        authenticator: Arc<dyn DeviceRequestAuthenticator>,
    ) -> io::Result<Self> {
        Self::start_authenticated(
            bind,
            dispatcher,
            max_concurrent_calls,
            Some(tls_acceptor),
            TransportAuthentication::Managed(authenticator),
        )
        .await
    }

    async fn start_authenticated(
        bind: SocketAddr,
        dispatcher: Arc<dyn PublishToolDispatcher>,
        max_concurrent_calls: u16,
        acceptor: Option<tokio_rustls::TlsAcceptor>,
        authentication: TransportAuthentication,
    ) -> io::Result<Self> {
        if (acceptor.is_none() && !bind.ip().is_loopback())
            || bind.ip().is_unspecified()
            || bind.ip().is_multicast()
            || !(1..=16).contains(&max_concurrent_calls)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid MCP listener configuration",
            ));
        }
        let listener = TcpListener::bind(bind).await?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            address,
            scheme: if acceptor.is_some() { "https" } else { "http" },
            authentication,
            dispatcher,
            cancel: CancellationToken::new(),
            sessions: Mutex::new(HashMap::new()),
            http_requests: Arc::new(Semaphore::new(MAX_HTTP_REQUESTS)),
            calls: Arc::new(Semaphore::new(max_concurrent_calls as usize)),
            records: Mutex::new(VecDeque::new()),
            workers: TaskTracker::new(),
        });
        let router = Router::new()
            .route("/mcp", any(handle))
            .with_state(shared.clone());
        let listener = BoundedListener {
            listener,
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            acceptor,
            handshakes: FuturesUnordered::new(),
        };
        let owner = shared.clone();
        let task = tokio::spawn(async move {
            let result = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<TlsPeer>(),
            )
            .with_graceful_shutdown(owner.cancel.clone().cancelled_owned())
            .await;
            owner.cancel();
            owner.workers.wait().await;
            result
        });
        let observer = PublishTransportObserver {
            shared: shared.clone(),
            task: task.abort_handle(),
        };
        Ok(Self {
            shared,
            task: Some(task),
            observer,
        })
    }

    pub fn endpoint(&self) -> String {
        format!("{}://{}/mcp", self.shared.scheme, self.shared.address)
    }

    pub fn snapshot(&self) -> PublishTransportSnapshot {
        self.observer.snapshot()
    }

    pub fn observation(&self) -> PublishTransportObserver {
        self.observer.clone()
    }

    pub fn cancel(&self) {
        self.shared.cancel();
    }

    pub async fn stop(&mut self) -> bool {
        self.cancel();
        let Some(task) = &mut self.task else {
            return true;
        };
        if tokio::time::timeout(DRAIN_TIMEOUT, task).await.is_err() {
            return false;
        }
        self.task.take();
        true
    }
}

impl Drop for PublishHttpServer {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn expire_sessions(sessions: &mut HashMap<String, Session>) {
    sessions.retain(|_, session| {
        if session.touched.elapsed() >= SESSION_IDLE {
            session.cancel.cancel();
            false
        } else {
            true
        }
    });
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}

fn header_gate(shared: &Shared, headers: &HeaderMap) -> Result<(), Response> {
    if shared.cancel.is_cancelled() {
        return Err(http_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_stopping",
        ));
    }
    if headers.len() > 64
        || headers
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len())
            .sum::<usize>()
            > MAX_HEADERS
    {
        return Err(http_error(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "headers_too_large",
        ));
    }
    let valid_host = |value: &str| host_matches(shared.address, shared.scheme, value);
    if !header(headers, "host").is_some_and(valid_host) {
        return Err(http_error(StatusCode::FORBIDDEN, "invalid_host"));
    }
    if matches!(shared.authentication, TransportAuthentication::Managed(_))
        && headers.contains_key("origin")
    {
        return Err(http_error(StatusCode::FORBIDDEN, "invalid_origin"));
    }
    if headers.contains_key("origin")
        && !header(headers, "origin")
            .and_then(|value| value.strip_prefix(&format!("{}://", shared.scheme)))
            .is_some_and(valid_host)
    {
        return Err(http_error(StatusCode::FORBIDDEN, "invalid_origin"));
    }
    let supplied = bearer(headers);
    let manual_mismatch = match &shared.authentication {
        TransportAuthentication::Manual(expected) => {
            let supplied_hash: [u8; 32] = Sha256::digest(supplied.unwrap_or("").as_bytes()).into();
            supplied_hash
                .iter()
                .zip(expected)
                .fold(0u8, |value, (left, right)| value | (left ^ right))
                != 0
        }
        TransportAuthentication::Managed(_) => false,
    };
    if supplied.is_none() || manual_mismatch {
        let mut response = http_error(StatusCode::UNAUTHORIZED, "unauthorized");
        response.headers_mut().insert(
            "www-authenticate",
            "Bearer realm=\"moyAI MCP publish\"".parse().unwrap(),
        );
        return Err(response);
    }
    if headers.contains_key("mcp-protocol-version")
        && header(headers, "mcp-protocol-version") != Some(PROTOCOL_VERSION)
    {
        return Err(http_error(
            StatusCode::BAD_REQUEST,
            "unsupported_protocol_version",
        ));
    }
    Ok(())
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    header(headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| {
            (32..=256).contains(&value.len())
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        })
}

/// These operation classes are selected from parsed MCP methods, never from an
/// HTTP header or a model-supplied permission claim.
fn authorization_action(method: &str, params: &Value) -> &'static str {
    match method {
        "initialize" | "notifications/initialized" | "ping" | "tools/list" => "observe",
        "notifications/cancelled" => "cancel",
        "tools/call" => match params.get("name").and_then(Value::as_str) {
            Some("task_status") => "observe",
            Some("cancel_task") => "cancel",
            _ => "execute",
        },
        _ => "execute",
    }
}

async fn authorize(
    shared: &Shared,
    headers: &HeaderMap,
    peer: &TlsPeer,
    action: &str,
) -> Result<Option<VerifiedGrant>, Response> {
    let TransportAuthentication::Managed(authenticator) = &shared.authentication else {
        return Ok(None);
    };
    let (Some(token), Some(fingerprint)) = (bearer(headers), peer.certificate_sha256.as_deref())
    else {
        return Err(http_error(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    tokio::select! {
        _ = shared.cancel.cancelled() => Err(http_error(StatusCode::SERVICE_UNAVAILABLE, "server_stopping")),
        result = tokio::time::timeout(Duration::from_secs(10), authenticator.authenticate(token, fingerprint, action)) => {
            match result {
                Ok(Ok(authority)) => Ok(Some(authority)),
                _ => Err(http_error(StatusCode::FORBIDDEN, "grant_denied")),
            }
        }
    }
}

fn host_matches(address: SocketAddr, scheme: &str, value: &str) -> bool {
    let literal = address.to_string();
    let localhost = format!("localhost:{}", address.port());
    if value.eq_ignore_ascii_case(&literal)
        || (address.ip().is_loopback() && value.eq_ignore_ascii_case(&localhost))
    {
        return true;
    }
    let default_port = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return false,
    };
    if address.port() != default_port {
        return false;
    }
    let host = match address.ip() {
        std::net::IpAddr::V4(ip) => ip.to_string(),
        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
    };
    value.eq_ignore_ascii_case(&host)
        || (address.ip().is_loopback() && value.eq_ignore_ascii_case("localhost"))
}

async fn handle(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    if let Err(response) = header_gate(&shared, request.headers()) {
        return response;
    }
    let Ok(_http_slot) = shared.http_requests.clone().try_acquire_owned() else {
        return http_error(StatusCode::TOO_MANY_REQUESTS, "request_capacity");
    };
    let (parts, body) = request.into_parts();
    let peer = parts
        .extensions
        .get::<ConnectInfo<TlsPeer>>()
        .map(|value| value.0.clone())
        .unwrap_or_default();
    let nonpost_authority = if parts.method != Method::POST {
        match authorize(
            &shared,
            &parts.headers,
            &peer,
            if parts.method == Method::DELETE {
                "cancel"
            } else {
                "observe"
            },
        )
        .await
        {
            Ok(authority) => authority,
            Err(response) => return response,
        }
    } else {
        None
    };
    if parts.method == Method::GET {
        let mut response = http_error(StatusCode::METHOD_NOT_ALLOWED, "sse_not_supported");
        response
            .headers_mut()
            .insert("allow", "POST, DELETE".parse().unwrap());
        return response;
    }
    if parts.method == Method::DELETE {
        let session_id = match session_header(&parts.headers) {
            Ok(id) => id,
            Err(response) => return response,
        };
        let mut sessions = shared.sessions.lock().expect("MCP sessions poisoned");
        expire_sessions(&mut sessions);
        if sessions.get(session_id).is_some_and(|session| {
            session.authority.as_ref() != nonpost_authority.as_ref().map(VerifiedGrant::claims)
        }) {
            return http_error(StatusCode::FORBIDDEN, "session_authority_mismatch");
        }
        return if let Some(session) = sessions.remove(session_id) {
            session.cancel.cancel();
            StatusCode::NO_CONTENT.into_response()
        } else {
            http_error(StatusCode::NOT_FOUND, "session_expired")
        };
    }
    if parts.method != Method::POST {
        return http_error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    if !header(&parts.headers, "content-type").is_some_and(|value| {
        value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    }) {
        return http_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    let accept = header(&parts.headers, "accept").unwrap_or("");
    let accepts = |kind: &str| {
        accept.split(',').any(|entry| {
            entry
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case(kind)
        })
    };
    if !accepts("application/json") || !accepts("text/event-stream") {
        return http_error(StatusCode::NOT_ACCEPTABLE, "accept_json_and_event_stream");
    }
    let bytes = tokio::select! {
        _ = shared.cancel.cancelled() => return http_error(StatusCode::SERVICE_UNAVAILABLE, "server_stopping"),
        result = tokio::time::timeout(BODY_TIMEOUT, to_bytes(body, MAX_BODY)) => match result {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => return http_error(StatusCode::PAYLOAD_TOO_LARGE, "request_body_limit"),
            Err(_) => return http_error(StatusCode::REQUEST_TIMEOUT, "request_body_timeout"),
        }
    };
    let message: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return rpc_error(StatusCode::BAD_REQUEST, None, -32700, "Parse error"),
    };
    let Some(object) = message.as_object() else {
        return rpc_error(StatusCode::BAD_REQUEST, None, -32600, "Invalid Request");
    };
    let id = object.get("id");
    let id_key = id.and_then(request_id);
    let method = object.get("method").and_then(Value::as_str);
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || method.is_none_or(|method| method.is_empty() || method.len() > 128)
        || id.is_some() && id_key.is_none()
        || object
            .get("params")
            .is_some_and(|params| !params.is_object())
        || object
            .keys()
            .any(|key| !["jsonrpc", "id", "method", "params"].contains(&key.as_str()))
    {
        return rpc_error(StatusCode::BAD_REQUEST, None, -32600, "Invalid Request");
    }
    let method = method.unwrap();
    let empty = json!({});
    let params = object.get("params").unwrap_or(&empty);
    let authority = match authorize(
        &shared,
        &parts.headers,
        &peer,
        authorization_action(method, params),
    )
    .await
    {
        Ok(authority) => authority,
        Err(response) => return response,
    };
    if method == "initialize" {
        return initialize(
            &shared,
            &parts.headers,
            id,
            id_key,
            params,
            authority.as_ref().map(|value| value.claims().clone()),
        );
    }
    let session_id = match session_header(&parts.headers) {
        Ok(id) => id.to_owned(),
        Err(response) => return response,
    };
    let (id, cancel, receive) = {
        let mut sessions = shared.sessions.lock().expect("MCP sessions poisoned");
        expire_sessions(&mut sessions);
        if shared.cancel.is_cancelled() {
            return http_error(StatusCode::SERVICE_UNAVAILABLE, "server_stopping");
        }
        let Some(session) = sessions.get_mut(&session_id) else {
            return http_error(StatusCode::NOT_FOUND, "session_expired");
        };
        if session.authority.as_ref() != authority.as_ref().map(VerifiedGrant::claims) {
            return http_error(StatusCode::FORBIDDEN, "session_authority_mismatch");
        }
        session.touched = Instant::now();
        if id.is_none() {
            match method {
                "notifications/initialized" => session.initialized = true,
                "notifications/cancelled" => {
                    if let Some(key) = params.get("requestId").and_then(request_id) {
                        if let Some(cancel) = session.active.get(&key) {
                            cancel.cancel();
                        }
                    }
                }
                _ => {}
            }
            return StatusCode::ACCEPTED.into_response();
        }
        let id = id.unwrap().clone();
        let key = id_key.unwrap();
        if session.used_ids.contains(&key) {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                Some(&id),
                -32600,
                "Request ID already used",
            );
        }
        if session.used_ids.len() >= MAX_REQUEST_IDS {
            session.cancel.cancel();
            sessions.remove(&session_id);
            return http_error(StatusCode::NOT_FOUND, "session_expired");
        }
        session.used_ids.insert(key.clone());
        if method == "ping" {
            return rpc_result(&id, json!({}));
        }
        if !session.initialized {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                Some(&id),
                -32000,
                "Session not initialized",
            );
        }
        if method == "tools/list" {
            if params.get("cursor").is_some() {
                return rpc_error(StatusCode::OK, Some(&id), -32602, "Invalid params");
            }
            return rpc_result(&id, json!({"tools":shared.dispatcher.tool_descriptors()}));
        }
        if method != "tools/call" {
            return rpc_error(StatusCode::OK, Some(&id), -32601, "Method not found");
        }
        let Some(name) = params
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 128)
        else {
            return rpc_error(StatusCode::OK, Some(&id), -32602, "Invalid params");
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return rpc_error(StatusCode::OK, Some(&id), -32602, "Invalid params");
        }
        // Only published descriptor names enter metadata audit. Arbitrary caller text
        // is never recorded as a tool name; dispatcher still rechecks its authority.
        if !shared
            .dispatcher
            .tool_descriptors()
            .iter()
            .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        {
            return rpc_error(
                StatusCode::OK,
                Some(&id),
                -32602,
                "Invalid tool or arguments",
            );
        }
        let Ok(slot) = shared.calls.clone().try_acquire_owned() else {
            return rpc_error(
                StatusCode::TOO_MANY_REQUESTS,
                Some(&id),
                -32000,
                "Tool capacity reached",
            );
        };
        let cancel = session.cancel.child_token();
        session.active.insert(key.clone(), cancel.clone());
        let record_id = ulid::Ulid::new().to_string();
        {
            let mut records = shared.records.lock().expect("MCP records poisoned");
            if records.len() == MAX_RECENT_CALLS {
                let oldest_finished = records
                    .iter()
                    .position(|record| record.slot.is_none())
                    .expect("active calls are bounded below audit capacity");
                records.remove(oldest_finished);
            }
            records.push_back(CallRecord {
                view: PublishCallRecord {
                    id: record_id.clone(),
                    tool: name.into(),
                    status: "running",
                },
                cancel: cancel.clone(),
                slot: Some(slot),
            });
        }
        let (send, receive) = oneshot::channel();
        let mut guard = ActiveCall {
            shared: shared.clone(),
            session: session_id,
            key,
            record_id,
            status: "failed",
        };
        let dispatcher = shared.dispatcher.clone();
        let name = name.to_owned();
        let worker_cancel = cancel.clone();
        // A detached tracked worker outlives HTTP disconnection and keeps its capacity
        // until the real tool future settles, including uninterruptible blocking I/O.
        shared.workers.spawn(async move {
            let call = async {
                match authority {
                    Some(authority) => {
                        dispatcher
                            .call_authorized(&name, arguments, worker_cancel.clone(), authority)
                            .await
                    }
                    None => {
                        dispatcher
                            .call(&name, arguments, worker_cancel.clone())
                            .await
                    }
                }
            };
            tokio::pin!(call);
            let result = tokio::select! {
                result = &mut call => result,
                _ = tokio::time::sleep(CALL_TIMEOUT) => {
                    worker_cancel.cancel();
                    // Cancellation cannot force synchronous OS I/O to finish. Await
                    // actual settlement without releasing the slot or leaking output.
                    let _ = call.await;
                    Err(PublishCallError::Cancelled)
                }
            };
            guard.status = if worker_cancel.is_cancelled()
                || matches!(result, Err(PublishCallError::Cancelled))
            {
                "cancelled"
            } else if result
                .as_ref()
                .is_ok_and(|value| value.get("isError") != Some(&Value::Bool(true)))
            {
                "completed"
            } else {
                "failed"
            };
            drop(guard);
            let _ = send.send(result);
        });
        (id, cancel, receive)
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => StatusCode::ACCEPTED.into_response(),
        result = receive => match result {
            Ok(Ok(value)) => rpc_result(&id, value),
            Ok(Err(error)) => dispatch_error(&id, error),
            Err(_) => rpc_error(StatusCode::OK, Some(&id), -32603, "Internal error"),
        },
        _ = tokio::time::sleep(CALL_TIMEOUT) => {
            cancel.cancel();
            rpc_error(StatusCode::OK, Some(&id), -32000, "Tool request timed out")
        }
    }
}

fn session_header(headers: &HeaderMap) -> Result<&str, Response> {
    if header(headers, "mcp-protocol-version") != Some(PROTOCOL_VERSION) {
        return Err(http_error(
            StatusCode::BAD_REQUEST,
            "protocol_version_required",
        ));
    }
    header(headers, "mcp-session-id")
        .filter(|id| {
            !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| (0x21..=0x7e).contains(&b))
        })
        .ok_or_else(|| http_error(StatusCode::BAD_REQUEST, "session_required"))
}

fn initialize(
    shared: &Arc<Shared>,
    headers: &HeaderMap,
    id: Option<&Value>,
    key: Option<String>,
    params: &Value,
    authority: Option<GrantClaims>,
) -> Response {
    let Some(id) = id else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            None,
            -32600,
            "Initialize requires a request ID",
        );
    };
    if headers.contains_key("mcp-session-id") {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            Some(id),
            -32600,
            "Initialize requires a new session",
        );
    }
    let nonempty = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty() && value.len() <= 128)
    };
    if !nonempty(params.get("protocolVersion"))
        || !params.get("capabilities").is_some_and(Value::is_object)
        || !nonempty(params.get("clientInfo").and_then(|info| info.get("name")))
        || !nonempty(
            params
                .get("clientInfo")
                .and_then(|info| info.get("version")),
        )
    {
        return rpc_error(
            StatusCode::OK,
            Some(id),
            -32602,
            "Invalid initialization params",
        );
    }
    let mut sessions = shared.sessions.lock().expect("MCP sessions poisoned");
    expire_sessions(&mut sessions);
    if shared.cancel.is_cancelled() {
        return http_error(StatusCode::SERVICE_UNAVAILABLE, "server_stopping");
    }
    if sessions.len() >= MAX_SESSIONS {
        return http_error(StatusCode::TOO_MANY_REQUESTS, "session_capacity");
    }
    let session_id = format!("{}{}", ulid::Ulid::new(), ulid::Ulid::new());
    sessions.insert(
        session_id.clone(),
        Session {
            authority,
            initialized: false,
            touched: Instant::now(),
            used_ids: key.into_iter().collect(),
            active: HashMap::new(),
            cancel: shared.cancel.child_token(),
        },
    );
    let mut response = rpc_result(
        id,
        json!({
            "protocolVersion":PROTOCOL_VERSION,
            "capabilities":{"tools":{"listChanged":false}},
            "serverInfo":{"name":"moyAI Desktop","version":env!("CARGO_PKG_VERSION")}
        }),
    );
    response
        .headers_mut()
        .insert("mcp-session-id", session_id.parse().unwrap());
    response
}

struct ActiveCall {
    shared: Arc<Shared>,
    session: String,
    key: String,
    record_id: String,
    status: &'static str,
}
impl Drop for ActiveCall {
    fn drop(&mut self) {
        if let Some(session) = self
            .shared
            .sessions
            .lock()
            .expect("MCP sessions poisoned")
            .get_mut(&self.session)
        {
            session.active.remove(&self.key);
        }
        if let Some(record) = self
            .shared
            .records
            .lock()
            .expect("MCP records poisoned")
            .iter_mut()
            .find(|record| record.view.id == self.record_id)
        {
            record.view.status = self.status;
            record.slot.take();
        }
    }
}

fn request_id(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if value.len() <= 128 => Some(format!("s:{value}")),
        Value::Number(number) if number.is_i64() || number.is_u64() => Some(format!("n:{number}")),
        _ => None,
    }
}

fn dispatch_error(id: &Value, error: PublishCallError) -> Response {
    match error {
        PublishCallError::InvalidArguments | PublishCallError::ToolUnavailable => rpc_error(
            StatusCode::OK,
            Some(id),
            -32602,
            "Invalid tool or arguments",
        ),
        PublishCallError::Cancelled => StatusCode::ACCEPTED.into_response(),
        _ => rpc_error(StatusCode::OK, Some(id), -32000, "Tool target unavailable"),
    }
}

fn http_error(status: StatusCode, code: &'static str) -> Response {
    json_response(status, json!({"error":code}))
}
fn rpc_error(status: StatusCode, id: Option<&Value>, code: i64, message: &'static str) -> Response {
    let mut response = json!({"jsonrpc":"2.0","error":{"code":code,"message":message}});
    if let Some(id) = id {
        response["id"] = id.clone();
    }
    json_response(status, response)
}
fn rpc_result(id: &Value, result: Value) -> Response {
    json_response(
        StatusCode::OK,
        json!({"jsonrpc":"2.0","id":id,"result":result}),
    )
}

struct BoundedJson(Vec<u8>);
impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_RESPONSE {
            return Err(io::Error::other("response limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn json_response(status: StatusCode, value: Value) -> Response {
    let mut bytes = BoundedJson(Vec::new());
    if serde_json::to_writer(&mut bytes, &value).is_err() {
        return rpc_error(
            StatusCode::OK,
            value.get("id"),
            -32603,
            "Response exceeds server limit",
        );
    }
    let mut response = (status, Body::from(bytes.0)).into_response();
    response
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("mcp-protocol-version", PROTOCOL_VERSION.parse().unwrap());
    response
}

struct BoundedListener {
    listener: TcpListener,
    slots: Arc<Semaphore>,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
    handshakes: FuturesUnordered<Handshake>,
}

#[derive(Clone, Default)]
struct TlsPeer {
    certificate_sha256: Option<String>,
}

impl Connected<axum::serve::IncomingStream<'_, BoundedListener>> for TlsPeer {
    fn connect_info(stream: axum::serve::IncomingStream<'_, BoundedListener>) -> Self {
        stream.io().peer.clone()
    }
}

trait PublishSocket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> PublishSocket for T {}
type Handshake = Pin<Box<dyn Future<Output = io::Result<(BoundedIo, SocketAddr)>> + Send>>;
impl axum::serve::Listener for BoundedListener {
    type Io = BoundedIo;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                completed = self.handshakes.next(), if !self.handshakes.is_empty() => {
                    if let Some(Ok(connection)) = completed { return connection; }
                }
                incoming = self.listener.accept() => match incoming {
                Ok((stream, address)) => {
                    if let Ok(slot) = self.slots.clone().try_acquire_owned() {
                        if let Some(acceptor) = self.acceptor.clone() {
                            self.handshakes.push(Box::pin(async move {
                                let stream = tokio::time::timeout(Duration::from_secs(5), acceptor.accept(stream))
                                    .await.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
                                let peer = TlsPeer { certificate_sha256: stream.get_ref().1.peer_certificates()
                                    .and_then(|certificates| certificates.first())
                                    .map(|certificate| format!("{:x}", Sha256::digest(certificate.as_ref()))) };
                                Ok((BoundedIo {
                                    stream: Box::new(stream), _slot: slot,
                                    idle: Box::pin(tokio::time::sleep(CONNECTION_IDLE)),
                                    peer,
                                }, address))
                            }));
                        } else { return (
                            BoundedIo {
                                stream: Box::new(stream),
                                _slot: slot,
                                idle: Box::pin(tokio::time::sleep(CONNECTION_IDLE)),
                                peer: TlsPeer::default(),
                            },
                            address,
                        ); }
                    } else {
                        drop(stream);
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

struct BoundedIo {
    stream: Box<dyn PublishSocket>,
    _slot: OwnedSemaphorePermit,
    idle: Pin<Box<Sleep>>,
    peer: TlsPeer,
}
impl BoundedIo {
    fn poll_idle(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.idle.as_mut().poll(cx).is_ready() {
            Err(io::ErrorKind::TimedOut.into())
        } else {
            Ok(())
        }
    }
    fn progress(&mut self) {
        self.idle.as_mut().reset(Instant::now() + CONNECTION_IDLE);
    }
}
impl AsyncRead for BoundedIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(cx) {
            return Poll::Ready(Err(error));
        }
        let before = buffer.filled().len();
        let result = Pin::new(&mut this.stream).poll_read(cx, buffer);
        if buffer.filled().len() > before {
            this.progress();
        }
        result
    }
}
impl AsyncWrite for BoundedIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(cx) {
            return Poll::Ready(Err(error));
        }
        let result = Pin::new(&mut this.stream).poll_write(cx, bytes);
        if matches!(result, Poll::Ready(Ok(count)) if count > 0) {
            this.progress();
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod managed_tests;
#[cfg(test)]
mod tests;
