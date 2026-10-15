use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::config::{McpConfig, McpServerConfig};
use crate::error::ToolError;
use crate::tool::ToolEffectClass;
use crate::tool::truncate::clip_text_with_ellipsis;

pub const MCP_TOOLS_LIST_DESCRIPTOR_SCHEMA_VALIDATION_MARKER: &str =
    "mcp_tools_list_descriptor_schema_validation";
const MAX_MCP_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_MCP_REQUEST_ENDPOINT_BYTES: usize = 8 * 1024;
const MAX_MCP_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const MAX_MCP_REQUEST_HEADER_COUNT: usize = 64;
const MAX_MCP_REQUEST_HEADER_BYTES: usize = 64 * 1024;
const MAX_MCP_REQUEST_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;
const STREAMABLE_PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Clone, PartialEq, Eq)]
struct NegotiatedSession {
    id: Option<String>,
}

impl std::fmt::Debug for NegotiatedSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NegotiatedSession { credential: [redacted] }")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConnectedEndpoint {
    endpoint: String,
    /// None preserves legacy endpoints that accept tools/list without initialization.
    session: Option<NegotiatedSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    pub effect: ToolEffectClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
}

/// Returns the configured effect for a concrete MCP call. Listing tools is a
/// read operation. A call is read-capable only when its exact server/tool route
/// says so; missing or malformed routing information fails closed.
pub fn effect_for_raw_call(config: &McpConfig, raw_arguments: &Value) -> ToolEffectClass {
    let Some(server_id) = raw_arguments.get("server_id").and_then(Value::as_str) else {
        return ToolEffectClass::Destructive;
    };
    let tool_name = raw_arguments
        .get("tool_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let Some(tool_name) = tool_name else {
        return ToolEffectClass::Read;
    };
    config
        .servers
        .iter()
        .find(|server| server.id == server_id && server.enabled)
        .map(|server| configured_tool_effect(server, tool_name))
        .unwrap_or(ToolEffectClass::Destructive)
}

fn configured_tool_effect(server: &McpServerConfig, tool_name: &str) -> ToolEffectClass {
    let mut matches = server
        .tool_routes
        .iter()
        .filter(|route| route.name == tool_name);
    let Some(route) = matches.next() else {
        return ToolEffectClass::Destructive;
    };
    if matches.next().is_some() {
        return ToolEffectClass::Destructive;
    }
    route.effect
}

pub fn can_route_effect(config: &McpConfig, effect: ToolEffectClass) -> bool {
    config
        .servers
        .iter()
        .filter(|server| server.enabled)
        .any(|server| {
            effect == ToolEffectClass::Read
                || server
                    .tool_routes
                    .iter()
                    .any(|route| configured_tool_effect(server, &route.name) == effect)
                || (effect == ToolEffectClass::Destructive && server.tool_routes.is_empty())
        })
}

#[derive(Debug, Clone)]
pub enum McpOperationResult {
    ToolsListed {
        server_id: String,
        endpoint: String,
        tools: Vec<McpToolDescriptor>,
    },
    ToolCalled {
        server_id: String,
        endpoint: String,
        tool_name: String,
        output_text: String,
        raw_result: Value,
    },
}

#[derive(Clone)]
pub struct McpClient {
    config: McpConfig,
    http: HashMap<String, Result<reqwest::Client, String>>,
    connections: Arc<HashMap<String, tokio::sync::Mutex<Option<ConnectedEndpoint>>>>,
    request_ids: Arc<AtomicU64>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpClient")
            .field("configured_servers", &self.config.servers.len())
            .finish_non_exhaustive()
    }
}

impl McpClient {
    pub fn new(config: McpConfig) -> Self {
        let connections = config
            .servers
            .iter()
            .map(|server| (server.id.clone(), tokio::sync::Mutex::new(None)))
            .collect();
        let http = config
            .servers
            .iter()
            .map(|server| {
                let client = (|| {
                    let mut builder =
                        reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
                    if let Some(pem) = &server.trusted_certificate_pem {
                        if pem.len() > 65536 {
                            return Err("MCP certificate size limit".into());
                        }
                        let cert = reqwest::Certificate::from_pem(pem.as_bytes())
                            .map_err(|_| "invalid MCP trusted certificate".to_string())?;
                        // Trust belongs to this peer alone. Other connections do not
                        // inherit its certificate, and hostname validation stays on.
                        builder = builder
                            .tls_built_in_root_certs(false)
                            .add_root_certificate(cert);
                    }
                    builder
                        .build()
                        .map_err(|_| "MCP HTTP client configuration failed".into())
                })();
                (server.id.clone(), client)
            })
            .collect();
        Self {
            config,
            http,
            connections: Arc::new(connections),
            request_ids: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    pub(crate) fn with_runtime_http(mut self, server_id: &str, http: reqwest::Client) -> Self {
        if self.http.contains_key(server_id) {
            self.http.insert(server_id.to_string(), Ok(http));
        }
        self
    }

    /// A request-scoped credential view shares only the negotiated connection
    /// and request-ID owner. Concurrent operations cannot replace each other's
    /// authorization, and the cached client need not retain a bearer token.
    pub(crate) fn with_runtime_authorization(
        &self,
        server_id: &str,
        bearer: &str,
    ) -> Result<Self, ToolError> {
        self.server(server_id)?;
        let authorization = format!("Bearer {bearer}");
        HeaderValue::from_str(&authorization)
            .map_err(|_| ToolError::Message("invalid MCP authorization header".into()))?;
        let mut client = self.clone();
        let server = client
            .config
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .unwrap();
        server
            .headers
            .retain(|name, _| !name.eq_ignore_ascii_case("authorization"));
        server.headers.insert("Authorization".into(), authorization);
        Ok(client)
    }

    /// Release one negotiated session without retrying any tool operation.
    /// Local ownership is retired even if the receiver has already restarted
    /// or no longer accepts the current credential. The caller supplies fresh
    /// operation-scoped authorization before requesting this cleanup.
    pub(crate) async fn close_session(
        &self,
        server_id: &str,
        mut checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<(), ToolError> {
        let server = self.server(server_id)?;
        let deadline = mcp_operation_deadline(server)?.min(Instant::now() + Duration::from_secs(3));
        let mut current = tokio::time::timeout_at(deadline, self.connections[server_id].lock())
            .await
            .map_err(|_| mcp_deadline_error(server))?;
        let Some(connection) = current.take() else {
            return Ok(());
        };
        let Some(session) = connection.session.filter(|s| s.id.is_some()) else {
            return Ok(());
        };
        let prepared =
            prepare_mcp_session_request(server, &connection.endpoint, &json!({}), Some(&session))?;
        let http = self
            .http
            .get(server_id)
            .ok_or_else(|| ToolError::Message("MCP connection is unavailable".into()))?
            .as_ref()
            .map_err(|error| ToolError::Message(error.clone()))?;
        let mut request = http.delete(prepared.endpoint);
        for (name, value) in prepared.configured_headers {
            request = request.header(name, value);
        }
        checkpoint()?;
        let response = tokio::time::timeout_at(deadline, request.send())
            .await
            .map_err(|_| mcp_deadline_error(server))?
            .map_err(|_| ToolError::Message("MCP session cleanup failed".into()))?;
        if matches!(
            response.status(),
            reqwest::StatusCode::NO_CONTENT | reqwest::StatusCode::NOT_FOUND
        ) {
            Ok(())
        } else {
            Err(ToolError::Message(format!(
                "MCP session cleanup returned HTTP {}",
                response.status().as_u16()
            )))
        }
    }

    pub async fn list_tools(
        &self,
        server_id: &str,
        mut effect_checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpOperationResult, ToolError> {
        if !self.config.enabled {
            return Err(ToolError::Message("mcp is disabled by config".to_string()));
        }
        let server = self.server(server_id)?;
        let deadline = mcp_operation_deadline(server)?;

        let (connection, response) = self
            .resolve_endpoint_with_tools_list(server, deadline, &mut effect_checkpoint)
            .await?;
        let mut tools = parse_tools(&response)?;
        for tool in &mut tools {
            tool.effect = configured_tool_effect(server, &tool.name);
        }
        let filtered = if server.tool_routes.is_empty() {
            tools
        } else {
            tools
                .into_iter()
                .filter(|tool| {
                    server
                        .tool_routes
                        .iter()
                        .any(|route| route.name == tool.name)
                })
                .collect()
        };
        Ok(McpOperationResult::ToolsListed {
            server_id: server.id.clone(),
            endpoint: connection.endpoint,
            tools: filtered,
        })
    }

    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: Value,
        mut effect_checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpOperationResult, ToolError> {
        if !self.config.enabled {
            return Err(ToolError::Message("mcp is disabled by config".to_string()));
        }
        let server = self.server(server_id)?;
        let deadline = mcp_operation_deadline(server)?;
        if !server.tool_routes.is_empty()
            && !server
                .tool_routes
                .iter()
                .any(|route| route.name == tool_name)
        {
            return Err(ToolError::Message(format!(
                "mcp server `{}` does not allow tool `{tool_name}` in current config",
                server.id
            )));
        }

        let cached = tokio::time::timeout_at(deadline, self.connections[&server.id].lock())
            .await
            .map_err(|_| mcp_deadline_error(server))?
            .clone();
        let connection = if let Some(connection) = cached {
            connection
        } else {
            let (connection, response) = self
                .resolve_endpoint_with_tools_list(server, deadline, &mut effect_checkpoint)
                .await?;
            let tools = parse_tools(&response)?;
            if !tools.iter().any(|tool| tool.name == tool_name) {
                return Err(ToolError::Message(format!(
                    "mcp server `{}` did not advertise tool `{tool_name}` during endpoint resolution",
                    server.id
                )));
            }
            connection
        };
        // The effectful request has exactly one transport boundary. Endpoint
        // discovery is completed by a read-only tools/list call before this
        // point, so an ambiguous HTTP failure can never replay tools/call at a
        // fallback URL.
        let response = self
            .post_wire(
                server,
                &connection.endpoint,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": tool_name,
                        "arguments": arguments,
                    }
                }),
                connection.session.as_ref(),
                deadline,
                &mut effect_checkpoint,
            )
            .await?;
        if response.status == reqwest::StatusCode::NOT_FOUND && connection.session.is_some() {
            let mut current =
                tokio::time::timeout_at(deadline, self.connections[&server.id].lock())
                    .await
                    .map_err(|_| mcp_deadline_error(server))?;
            if current.as_ref() == Some(&connection) {
                *current = None;
            }
        }
        let response = response.rpc_value(connection.session.is_some())?;
        if response.get("error").is_some() {
            return Err(ToolError::Message(
                "mcp tools/call returned a protocol error".into(),
            ));
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| ToolError::Message("mcp response is missing `result`".to_string()))?;
        Ok(McpOperationResult::ToolCalled {
            server_id: server.id.clone(),
            endpoint: connection.endpoint,
            tool_name: tool_name.to_string(),
            output_text: render_tool_call_output(&result),
            raw_result: result,
        })
    }

    fn server(&self, server_id: &str) -> Result<&McpServerConfig, ToolError> {
        if self
            .config
            .servers
            .iter()
            .filter(|server| server.id == server_id)
            .count()
            != 1
        {
            return Err(ToolError::Message(
                "unknown or ambiguous MCP connection ID".into(),
            ));
        }
        let server = self
            .config
            .servers
            .iter()
            .find(|server| server.id == server_id)
            .ok_or_else(|| ToolError::Message(format!("unknown mcp server `{server_id}`")))?;
        if !server.enabled {
            return Err(ToolError::Message(format!(
                "mcp server `{server_id}` is disabled by config"
            )));
        }
        Ok(server)
    }

    #[cfg(test)]
    async fn post_json(
        &self,
        server: &McpServerConfig,
        endpoint: &str,
        payload: Value,
        deadline: Instant,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<Value, ToolError> {
        self.post_wire(server, endpoint, payload, None, deadline, effect_checkpoint)
            .await?
            .rpc_value(false)
    }

    async fn post_wire(
        &self,
        server: &McpServerConfig,
        endpoint: &str,
        mut payload: Value,
        session: Option<&NegotiatedSession>,
        deadline: Instant,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpHttpReply, ToolError> {
        if payload.get("id").is_some() {
            let id = self
                .request_ids
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .map_err(|_| ToolError::Message("mcp request identity exhausted".into()))?;
            payload["id"] = json!(id);
        }
        let sent_id = payload.get("id").cloned();
        let prepared = prepare_mcp_session_request(server, endpoint, &payload, session)?;
        let mut request = self
            .http
            .get(&server.id)
            .ok_or_else(|| ToolError::Message("MCP connection is unavailable".into()))?
            .as_ref()
            .map_err(|error| ToolError::Message(error.clone()))?
            .post(prepared.endpoint.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json");
        for (name, value) in prepared.configured_headers {
            request = request.header(name, value);
        }

        let request = request.body(prepared.body);
        // Re-check the typed run owner at every actual send boundary. Endpoint
        // discovery may issue multiple read-only tools/list requests, while a
        // tools/call request reaches this boundary exactly once.
        effect_checkpoint()?;
        if Instant::now() >= deadline {
            return Err(mcp_deadline_error(server));
        }
        let endpoint = prepared.endpoint.to_string();
        tokio::time::timeout_at(deadline, async {
            let response = request
                .send()
                .await
                .map_err(|error| ToolError::Message(format!("mcp request failed: {error}")))?;
            let status = response.status();
            let session_header = response.headers().get("mcp-session-id").map(|value| {
                value
                    .to_str()
                    .ok()
                    .filter(|value| {
                        !value.is_empty()
                            && value.len() <= 128
                            && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
                    })
                    .map(str::to_owned)
            });
            let invalid_session_header = session_header.as_ref().is_some_and(Option::is_none)
                || response.headers().get_all("mcp-session-id").iter().count() > 1;
            if response
                .content_length()
                .is_some_and(|length| length > MAX_MCP_RESPONSE_BYTES as u64)
            {
                return Err(ToolError::Message(format!(
                    "mcp response from `{endpoint}` exceeds the {} byte limit",
                    MAX_MCP_RESPONSE_BYTES
                )));
            }
            let mut body_bytes = Vec::new();
            let mut body_stream = response.bytes_stream();
            while let Some(chunk) = body_stream.next().await {
                let chunk = chunk.map_err(|error| {
                    ToolError::Message(format!("failed to read mcp response body: {error}"))
                })?;
                append_bounded_response_chunk(&mut body_bytes, &chunk, &endpoint)?;
            }
            let body = String::from_utf8(body_bytes).map_err(|_| {
                ToolError::Message("mcp response body is not valid UTF-8".to_string())
            })?;
            let invalid_host = body.to_ascii_lowercase().contains("invalid host header");
            // Remote error bodies and session IDs are never diagnostic text. They
            // may echo credentials or tool input; only typed status reaches errors.
            let value = if body.trim().is_empty() {
                None
            } else {
                match parse_json_or_sse(&body) {
                    Ok(value) => Some(value),
                    Err(_) if !status.is_success() => None,
                    Err(_) => {
                        return Err(ToolError::Message(
                            "failed to parse mcp response body".into(),
                        ));
                    }
                }
            };
            Ok(McpHttpReply {
                status,
                value,
                sent_id,
                session_id: session_header.flatten(),
                invalid_session_header,
                invalid_host,
            })
        })
        .await
        .map_err(|_| mcp_deadline_error(server))?
    }

    async fn resolve_endpoint_with_tools_list(
        &self,
        server: &McpServerConfig,
        deadline: Instant,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<(ConnectedEndpoint, Value), ToolError> {
        // One owner per configured server serializes discovery/initialization, but
        // unrelated configured servers and already admitted tool calls stay independent.
        let mut current = tokio::time::timeout_at(deadline, self.connections[&server.id].lock())
            .await
            .map_err(|_| mcp_deadline_error(server))?;
        if let Some(connection) = current.clone() {
            let reply = self
                .post_wire(
                    server,
                    &connection.endpoint,
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
                    connection.session.as_ref(),
                    deadline,
                    effect_checkpoint,
                )
                .await?;
            if reply.status == reqwest::StatusCode::NOT_FOUND && connection.session.is_some() {
                *current = None;
            } else {
                let value = reply.rpc_value(connection.session.is_some())?;
                parse_tools(&value)?;
                return Ok((connection, value));
            }
        }
        let endpoints = endpoint_candidates(&server.base_url)?;
        if endpoints.is_empty() {
            return Err(ToolError::Message(
                "mcp endpoint is not configured".to_string(),
            ));
        }
        let mut last_error = None;
        for endpoint in endpoints {
            let discovered = self
                .post_wire(
                    server,
                    &endpoint,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/list",
                        "params": {}
                    }),
                    None,
                    deadline,
                    effect_checkpoint,
                )
                .await;
            match discovered {
                Ok(reply) => {
                    let (connection, response) =
                        if reply.status == reqwest::StatusCode::BAD_REQUEST {
                            // Probe lifecycle only after a read-only request is refused.
                            // Legacy successful tools/list never incurs an extra handshake.
                            let session = match self
                                .initialize(server, &endpoint, deadline, effect_checkpoint)
                                .await
                            {
                                Ok(session) => session,
                                Err(ToolError::RunInterrupted) => {
                                    return Err(ToolError::RunInterrupted);
                                }
                                Err(error) => {
                                    last_error = Some(error);
                                    continue;
                                }
                            };
                            let response = self.post_wire(server, &endpoint,
                            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
                            Some(&session), deadline, effect_checkpoint).await?.rpc_value(true)?;
                            (
                                ConnectedEndpoint {
                                    endpoint,
                                    session: Some(session),
                                },
                                response,
                            )
                        } else if reply.status.is_success() {
                            (
                                ConnectedEndpoint {
                                    endpoint,
                                    session: None,
                                },
                                reply.rpc_value(false)?,
                            )
                        } else {
                            last_error = Some(reply.rpc_value(false).unwrap_err());
                            continue;
                        };
                    parse_tools(&response)?;
                    *current = Some(connection.clone());
                    return Ok((connection, response));
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            ToolError::Message("mcp request failed without a detailed error".to_string())
        }))
    }

    async fn initialize(
        &self,
        server: &McpServerConfig,
        endpoint: &str,
        deadline: Instant,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<NegotiatedSession, ToolError> {
        let reply = self
            .post_wire(
                server,
                endpoint,
                json!({
                    "jsonrpc":"2.0","id":1,"method":"initialize","params":{
                        "protocolVersion":STREAMABLE_PROTOCOL_VERSION,"capabilities":{},
                        "clientInfo":{"name":"moyAI Desktop","version":env!("CARGO_PKG_VERSION")}
                    }
                }),
                None,
                deadline,
                effect_checkpoint,
            )
            .await?;
        if reply.invalid_session_header {
            return Err(ToolError::Message(
                "mcp initialize returned an invalid session header".into(),
            ));
        }
        let session = NegotiatedSession {
            id: reply.session_id.clone(),
        };
        let value = reply.rpc_value(true)?;
        let result = value
            .get("result")
            .ok_or_else(|| ToolError::Message("mcp initialize was rejected".into()))?;
        if result.get("protocolVersion").and_then(Value::as_str)
            != Some(STREAMABLE_PROTOCOL_VERSION)
            || !result
                .get("capabilities")
                .and_then(|caps| caps.get("tools"))
                .is_some_and(Value::is_object)
            || !result.get("serverInfo").is_some_and(Value::is_object)
        {
            return Err(ToolError::Message(
                "mcp initialize returned unsupported protocol or capabilities".into(),
            ));
        }
        let acknowledged = self
            .post_wire(
                server,
                endpoint,
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                Some(&session),
                deadline,
                effect_checkpoint,
            )
            .await?;
        if acknowledged.status != reqwest::StatusCode::ACCEPTED || acknowledged.value.is_some() {
            return Err(ToolError::Message(
                "mcp initialized notification was not accepted".into(),
            ));
        }
        Ok(session)
    }
}

struct McpHttpReply {
    status: reqwest::StatusCode,
    value: Option<Value>,
    sent_id: Option<Value>,
    session_id: Option<String>,
    invalid_session_header: bool,
    invalid_host: bool,
}

impl McpHttpReply {
    fn rpc_value(self, strict: bool) -> Result<Value, ToolError> {
        if !self.status.is_success() {
            let hint = if self.invalid_host {
                " Configure `[mcp.servers[].headers]` if this server requires a specific Host header."
            } else {
                ""
            };
            return Err(ToolError::Message(format!(
                "mcp request failed with HTTP {}.{hint}",
                self.status.as_u16()
            )));
        }
        let value = self
            .value
            .ok_or_else(|| ToolError::Message("mcp response body is empty".into()))?;
        if strict
            && (value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                || value.get("id") != self.sent_id.as_ref()
                || value.get("result").is_some() == value.get("error").is_some())
        {
            return Err(ToolError::Message(
                "mcp response does not match its request".into(),
            ));
        }
        Ok(value)
    }
}

#[derive(Debug)]
struct PreparedMcpRequest {
    endpoint: reqwest::Url,
    body: Vec<u8>,
    configured_headers: Vec<(HeaderName, HeaderValue)>,
}

fn prepare_mcp_session_request(
    server: &McpServerConfig,
    endpoint: &str,
    payload: &Value,
    session: Option<&NegotiatedSession>,
) -> Result<PreparedMcpRequest, ToolError> {
    let Some(session) = session else {
        return prepare_mcp_request(server, endpoint, payload);
    };
    let mut negotiated = server.clone();
    negotiated.headers.retain(|name, _| {
        !name.eq_ignore_ascii_case("mcp-session-id")
            && !name.eq_ignore_ascii_case("mcp-protocol-version")
    });
    negotiated.headers.insert(
        "MCP-Protocol-Version".into(),
        STREAMABLE_PROTOCOL_VERSION.into(),
    );
    if let Some(id) = &session.id {
        negotiated
            .headers
            .insert("MCP-Session-Id".into(), id.clone());
    }
    // Negotiated headers count toward the existing envelope/header budgets and
    // cannot be overridden by stale, differently-cased configuration headers.
    prepare_mcp_request(&negotiated, endpoint, payload)
}

fn mcp_operation_deadline(server: &McpServerConfig) -> Result<Instant, ToolError> {
    if server.timeout_ms == 0 {
        return Err(ToolError::Message(format!(
            "mcp server `{}` has a zero operation deadline",
            server.id
        )));
    }
    Instant::now()
        .checked_add(Duration::from_millis(server.timeout_ms))
        .ok_or_else(|| {
            ToolError::Message(format!(
                "mcp server `{}` operation deadline is out of range",
                server.id
            ))
        })
}

fn mcp_deadline_error(server: &McpServerConfig) -> ToolError {
    ToolError::Message(format!(
        "mcp operation for server `{}` exceeded its {} ms absolute deadline",
        server.id, server.timeout_ms
    ))
}

fn prepare_mcp_request(
    server: &McpServerConfig,
    endpoint: &str,
    payload: &Value,
) -> Result<PreparedMcpRequest, ToolError> {
    let endpoint = validate_mcp_endpoint_origin(server, endpoint)?;
    if endpoint.as_str().len() > MAX_MCP_REQUEST_ENDPOINT_BYTES {
        return Err(ToolError::Message(format!(
            "mcp endpoint exceeds the {MAX_MCP_REQUEST_ENDPOINT_BYTES} byte limit"
        )));
    }

    let body = serde_json::to_vec(payload).map_err(|_| {
        ToolError::Message("failed to serialize bounded mcp request body".to_string())
    })?;
    if body.len() > MAX_MCP_REQUEST_BODY_BYTES {
        return Err(ToolError::Message(format!(
            "mcp request body exceeds the {MAX_MCP_REQUEST_BODY_BYTES} byte limit"
        )));
    }

    let fixed_headers = [
        (ACCEPT.as_str(), "application/json, text/event-stream"),
        (CONTENT_TYPE.as_str(), "application/json"),
    ];
    if server.headers.len().saturating_add(fixed_headers.len()) > MAX_MCP_REQUEST_HEADER_COUNT {
        return Err(ToolError::Message(format!(
            "mcp request headers exceed the {MAX_MCP_REQUEST_HEADER_COUNT} field limit"
        )));
    }
    let mut header_bytes = fixed_headers
        .iter()
        .try_fold(0usize, |total, (name, value)| {
            total
                .checked_add(name.len())
                .and_then(|total| total.checked_add(value.len()))
                .and_then(|total| total.checked_add(4))
                .ok_or_else(|| ToolError::Message("mcp request header size overflowed".to_string()))
        })?;
    let mut configured_headers = Vec::with_capacity(server.headers.len());
    for (name, value) in &server.headers {
        let visible_name = clip_text_with_ellipsis(name, 128);
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            ToolError::Message(format!(
                "mcp request header name `{visible_name}` is invalid"
            ))
        })?;
        let mut value = HeaderValue::from_str(value).map_err(|_| {
            ToolError::Message(format!(
                "mcp request header `{visible_name}` has an invalid value"
            ))
        })?;
        if matches!(
            name.as_str(),
            "authorization" | "proxy-authorization" | "mcp-session-id"
        ) {
            value.set_sensitive(true);
        }
        header_bytes = header_bytes
            .checked_add(name.as_str().len())
            .and_then(|total| total.checked_add(value.as_bytes().len()))
            .and_then(|total| total.checked_add(4))
            .ok_or_else(|| ToolError::Message("mcp request header size overflowed".to_string()))?;
        if header_bytes > MAX_MCP_REQUEST_HEADER_BYTES {
            return Err(ToolError::Message(format!(
                "mcp request headers exceed the {MAX_MCP_REQUEST_HEADER_BYTES} byte limit"
            )));
        }
        configured_headers.push((name, value));
    }

    let envelope_bytes = endpoint
        .as_str()
        .len()
        .checked_add(body.len())
        .and_then(|total| total.checked_add(header_bytes))
        .and_then(|total| total.checked_add("POST ".len()))
        .ok_or_else(|| ToolError::Message("mcp request envelope size overflowed".to_string()))?;
    if envelope_bytes > MAX_MCP_REQUEST_ENVELOPE_BYTES {
        return Err(ToolError::Message(format!(
            "mcp serialized request exceeds the {MAX_MCP_REQUEST_ENVELOPE_BYTES} byte limit"
        )));
    }

    Ok(PreparedMcpRequest {
        endpoint,
        body,
        configured_headers,
    })
}

fn validate_mcp_endpoint_origin(
    server: &McpServerConfig,
    endpoint: &str,
) -> Result<reqwest::Url, ToolError> {
    let configured = crate::config::model::canonical_mcp_base_url(&server.base_url)
        .and_then(|canonical| {
            reqwest::Url::parse(&canonical).map_err(|_| "must be a valid absolute URL".to_string())
        })
        .map_err(|error| {
            ToolError::Message(format!(
                "mcp server `{}` has an invalid configured endpoint: {error}",
                server.id
            ))
        })?;
    let endpoint = crate::config::model::canonical_mcp_base_url(endpoint)
        .and_then(|canonical| {
            reqwest::Url::parse(&canonical).map_err(|_| "must be a valid absolute URL".to_string())
        })
        .map_err(|error| ToolError::Message(format!("mcp request endpoint is invalid: {error}")))?;
    if configured.origin() != endpoint.origin() {
        return Err(ToolError::Message(format!(
            "mcp request endpoint does not match the configured origin for server `{}`",
            server.id
        )));
    }
    Ok(endpoint)
}

fn append_bounded_response_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
    endpoint: &str,
) -> Result<(), ToolError> {
    if body.len().saturating_add(chunk.len()) > MAX_MCP_RESPONSE_BYTES {
        return Err(ToolError::Message(format!(
            "mcp response from `{endpoint}` exceeds the {} byte limit",
            MAX_MCP_RESPONSE_BYTES
        )));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn endpoint_candidates(base_url: &str) -> Result<Vec<String>, ToolError> {
    let canonical = crate::config::model::canonical_mcp_base_url(base_url)
        .map_err(|error| ToolError::Message(format!("invalid configured mcp endpoint: {error}")))?;
    let mut fallback = reqwest::Url::parse(&canonical)
        .map_err(|_| ToolError::Message("invalid configured mcp endpoint".to_string()))?;
    let path = fallback.path().trim_end_matches('/').to_string();
    if path.ends_with("/mcp") {
        return Ok(vec![canonical]);
    }
    let fallback_path = if path.is_empty() || path == "/" {
        "/mcp".to_string()
    } else {
        format!("{path}/mcp")
    };
    fallback.set_path(&fallback_path);
    Ok(vec![canonical, fallback.to_string()])
}

fn parse_json_or_sse(body: &str) -> Result<Value, ToolError> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return Ok(value);
    }

    let mut data_lines = Vec::new();
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            let trimmed = data.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                continue;
            }
            data_lines.push(trimmed.to_string());
        }
    }
    for candidate in data_lines.iter().rev() {
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            return Ok(value);
        }
    }
    if !data_lines.is_empty() {
        let merged = data_lines.join("");
        if let Ok(value) = serde_json::from_str::<Value>(&merged) {
            return Ok(value);
        }
    }

    Err(ToolError::Message(
        "failed to parse mcp response body".into(),
    ))
}

fn parse_tools(response: &Value) -> Result<Vec<McpToolDescriptor>, ToolError> {
    if response.get("error").is_some() {
        return Err(ToolError::Message(
            "mcp tools/list returned a protocol error".into(),
        ));
    }
    let tools = response
        .get("result")
        .and_then(|result| result.get("tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ToolError::Message("mcp tools/list response is missing `result.tools`".to_string())
        })?;
    let mut descriptors = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        descriptors.push(parse_tool_descriptor(index, tool)?);
    }
    Ok(descriptors)
}

fn parse_tool_descriptor(index: usize, tool: &Value) -> Result<McpToolDescriptor, ToolError> {
    let name = tool.get("name").and_then(Value::as_str).ok_or_else(|| {
        ToolError::Message(format!(
            "mcp tools/list descriptor at index {index} is missing typed string `name`"
        ))
    })?;
    if name.trim().is_empty() {
        return Err(ToolError::Message(format!(
            "mcp tools/list descriptor at index {index} has an empty `name`"
        )));
    }
    let description = match tool.get("description") {
        Some(value) => Some(value.as_str().ok_or_else(|| {
            ToolError::Message(format!(
                "mcp tools/list descriptor at index {index} has non-string `description`"
            ))
        })?),
        None => None,
    };
    Ok(McpToolDescriptor {
        name: name.to_string(),
        effect: ToolEffectClass::Destructive,
        description: description.map(str::to_string),
        input_schema: tool.get("inputSchema").cloned(),
        annotations: match tool.get("annotations") {
            Some(value) => Some(serde_json::from_value(value.clone()).map_err(|_| {
                ToolError::Message(format!(
                    "mcp tools/list descriptor at index {index} has invalid `annotations`"
                ))
            })?),
            None => None,
        },
    })
}

fn render_tool_call_output(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        let lines = content
            .iter()
            .filter_map(render_content_item)
            .collect::<Vec<_>>();
        if !lines.is_empty() {
            return lines.join("\n\n");
        }
    }
    if let Some(content) = result.get("structuredContent") {
        if let Ok(pretty) = serde_json::to_string_pretty(content) {
            return pretty;
        }
    }
    serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
}

fn render_content_item(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str) {
        Some("text") => item.get("text").and_then(Value::as_str).map(str::to_string),
        Some("image") => item
            .get("mimeType")
            .and_then(Value::as_str)
            .map(|mime| format!("[image content omitted: {mime}]")),
        Some("resource") => item
            .get("resource")
            .and_then(Value::as_object)
            .map(|resource| {
                let uri = resource
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or("resource");
                let text = resource
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| serde_json::to_string_pretty(resource).unwrap_or_default());
                format!("{uri}\n{text}")
            }),
        Some(_) | None => serde_json::to_string_pretty(item).ok(),
    }
}

pub fn mcp_tools_list_rejects_malformed_tool_descriptors_fixture_passes() -> bool {
    let valid = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": [
                {
                    "name": "inspect_repo",
                    "description": "Inspect repository state",
                    "inputSchema": {
                        "type": "object"
                    }
                }
            ]
        }
    });
    let malformed = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": [
                {
                    "description": "descriptor without a typed name",
                    "inputSchema": {
                        "type": "object"
                    }
                }
            ]
        }
    });
    let non_string_name = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": [
                {
                    "name": 42,
                    "description": "non-string name"
                }
            ]
        }
    });

    MCP_TOOLS_LIST_DESCRIPTOR_SCHEMA_VALIDATION_MARKER
        == "mcp_tools_list_descriptor_schema_validation"
        && parse_tools(&valid)
            .map(|tools| tools.len() == 1 && tools[0].name == "inspect_repo")
            .unwrap_or(false)
        && parse_tools(&malformed).is_err()
        && parse_tools(&non_string_name).is_err()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Json;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::{any, post};

    use super::*;

    fn routed_config() -> McpConfig {
        McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                display_name: None,
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: "http://mcp.invalid".to_string(),
                timeout_ms: 2_000,
                remote_agent: false,
                trusted_certificate_pem: None,
                tool_routes: vec![
                    crate::config::McpToolRouteConfig {
                        name: "inspect".to_string(),
                        effect: ToolEffectClass::Read,
                    },
                    crate::config::McpToolRouteConfig {
                        name: "change".to_string(),
                        effect: ToolEffectClass::Mutation,
                    },
                ],
                headers: BTreeMap::new(),
            }],
        }
    }

    #[test]
    fn runtime_authorization_is_request_scoped_and_keeps_one_connection_owner() {
        let mut config = routed_config();
        config.servers[0]
            .headers
            .insert("authorization".into(), "Bearer old-configured-value".into());
        let base = McpClient::new(config);
        let first = base
            .with_runtime_authorization("fixture", "first-runtime-token")
            .unwrap();
        let second = base
            .with_runtime_authorization("fixture", "second-runtime-token")
            .unwrap();
        assert!(Arc::ptr_eq(&base.connections, &first.connections));
        assert!(Arc::ptr_eq(&base.request_ids, &second.request_ids));
        assert_eq!(
            base.config.servers[0].headers["authorization"],
            "Bearer old-configured-value"
        );
        assert_eq!(
            first.config.servers[0].headers["Authorization"],
            "Bearer first-runtime-token"
        );
        assert_eq!(
            second.config.servers[0].headers["Authorization"],
            "Bearer second-runtime-token"
        );
        assert_eq!(first.config.servers[0].headers.len(), 1);
        assert!(
            base.with_runtime_authorization("fixture", "secret\r\ninvalid")
                .unwrap_err()
                .to_string()
                .find("secret")
                .is_none()
        );
        assert!(!format!("{first:?}").contains("first-runtime-token"));
    }

    #[test]
    fn negotiated_headers_and_diagnostics_keep_session_and_response_secrets_private() {
        const SECRET: &str = "session-and-response-secret";
        let mut config = routed_config();
        config.servers[0]
            .headers
            .insert("authorization".into(), SECRET.into());
        config.servers[0]
            .headers
            .insert("mcp-session-id".into(), "stale-session".into());
        config.servers[0]
            .headers
            .insert("mcp-protocol-version".into(), "stale-version".into());
        let session = NegotiatedSession {
            id: Some(SECRET.into()),
        };
        let request = prepare_mcp_session_request(
            &config.servers[0],
            "http://mcp.invalid",
            &json!({}),
            Some(&session),
        )
        .unwrap();
        assert_eq!(
            request
                .configured_headers
                .iter()
                .filter(|(name, _)| name == "mcp-session-id")
                .count(),
            1
        );
        assert!(
            request
                .configured_headers
                .iter()
                .any(|(name, value)| name == "mcp-session-id" && value == SECRET)
        );
        assert!(!format!("{request:?}").contains(SECRET));
        assert!(!format!("{session:?}").contains(SECRET));
        assert!(!format!("{:?}", McpClient::new(config)).contains(SECRET));
        assert!(
            !parse_json_or_sse(SECRET)
                .unwrap_err()
                .to_string()
                .contains(SECRET)
        );
        assert!(
            !parse_tools(&json!({"error":{"message":SECRET}}))
                .unwrap_err()
                .to_string()
                .contains(SECRET)
        );
        let mismatch = McpHttpReply {
            status: reqwest::StatusCode::OK,
            value: Some(json!({"jsonrpc":"2.0","id":SECRET,"result":{}})),
            sent_id: Some(json!(1)),
            session_id: None,
            invalid_session_header: false,
            invalid_host: false,
        }
        .rpc_value(true)
        .unwrap_err();
        assert!(!mismatch.to_string().contains(SECRET));
    }

    #[tokio::test]
    async fn legacy_bad_request_root_can_still_fall_back_to_a_working_tools_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(|| async { StatusCode::BAD_REQUEST }))
            .route(
                "/mcp",
                post(|| async {
                    Json(json!({"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"inspect"}]}}))
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = routed_config();
        config.servers[0].base_url = format!("http://{address}");
        let client = McpClient::new(config);
        let result = client.list_tools("fixture", || Ok(())).await.unwrap();
        assert!(
            matches!(result, McpOperationResult::ToolsListed { tools, .. } if tools.len() == 1)
        );
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn configured_routes_are_the_only_read_authority_for_mcp_calls() {
        let config = routed_config();
        assert_eq!(
            effect_for_raw_call(
                &config,
                &json!({"server_id": "fixture", "tool_name": "inspect"}),
            ),
            ToolEffectClass::Read
        );
        assert_eq!(
            effect_for_raw_call(
                &config,
                &json!({"server_id": "fixture", "tool_name": "change"}),
            ),
            ToolEffectClass::Mutation
        );
        for arguments in [
            json!({"server_id": "fixture", "tool_name": "unknown"}),
            json!({"server_id": "missing", "tool_name": "inspect"}),
            json!({"tool_name": "inspect"}),
        ] {
            assert_eq!(
                effect_for_raw_call(&config, &arguments),
                ToolEffectClass::Destructive
            );
        }
        assert_eq!(
            effect_for_raw_call(&config, &json!({"server_id": "fixture"})),
            ToolEffectClass::Read
        );

        let mut ambiguous = config;
        ambiguous.servers[0]
            .tool_routes
            .push(crate::config::McpToolRouteConfig {
                name: "inspect".to_string(),
                effect: ToolEffectClass::Destructive,
            });
        assert_eq!(
            effect_for_raw_call(
                &ambiguous,
                &json!({"server_id": "fixture", "tool_name": "inspect"}),
            ),
            ToolEffectClass::Destructive
        );
    }

    #[test]
    fn remote_read_only_hint_does_not_promote_an_unconfigured_route() {
        let tools = parse_tools(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [{
                    "name": "remote_claims_read",
                    "annotations": {
                        "readOnlyHint": true,
                        "destructiveHint": false
                    }
                }]
            }
        }))
        .expect("descriptor");

        assert_eq!(tools[0].effect, ToolEffectClass::Destructive);
        assert_eq!(
            tools[0]
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.read_only_hint),
            Some(true)
        );
    }

    #[test]
    fn streamed_response_limit_is_enforced_without_content_length() {
        let mut body = vec![b'a'; MAX_MCP_RESPONSE_BYTES - 1];
        append_bounded_response_chunk(&mut body, b"b", "http://mcp").expect("exact limit");
        let error =
            append_bounded_response_chunk(&mut body, b"c", "http://mcp").expect_err("over limit");

        assert_eq!(body.len(), MAX_MCP_RESPONSE_BYTES);
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn request_preparation_rejects_cross_origin_endpoints_and_bounded_inputs() {
        let mut config = routed_config();
        let server = &mut config.servers[0];
        server.base_url = "https://mcp.example.test/rpc".to_string();
        server
            .headers
            .insert("Authorization".to_string(), "do-not-disclose".to_string());

        let cross_origin = prepare_mcp_request(
            server,
            "https://other.example.test/rpc",
            &json!({"value": 1}),
        )
        .expect_err("configured request material must not cross origins");
        assert!(cross_origin.to_string().contains("configured origin"));
        assert!(!cross_origin.to_string().contains("do-not-disclose"));

        server.headers.clear();
        let oversized_body = prepare_mcp_request(
            server,
            "https://mcp.example.test/rpc",
            &json!({"value": "x".repeat(MAX_MCP_REQUEST_BODY_BYTES)}),
        )
        .expect_err("oversized serialized MCP body must fail before send");
        assert!(oversized_body.to_string().contains("request body"));

        server.headers.insert(
            "X-Large".to_string(),
            format!("secret-value{}", "x".repeat(MAX_MCP_REQUEST_HEADER_BYTES)),
        );
        let oversized_headers =
            prepare_mcp_request(server, "https://mcp.example.test/rpc", &json!({"value": 1}))
                .expect_err("oversized MCP headers must fail before send");
        assert!(oversized_headers.to_string().contains("request headers"));
        assert!(!oversized_headers.to_string().contains("secret-value"));
    }

    #[test]
    fn mcp_query_tokens_are_rejected_before_endpoint_preparation() {
        const URL_SECRET: &str = "mcp-url-secret-must-not-be-disclosed";

        let mut config = routed_config();
        let server = &mut config.servers[0];
        server.base_url = "https://mcp.example.test/rpc/".to_string();

        let prepared = prepare_mcp_request(
            server,
            "https://mcp.example.test/alternate/",
            &json!({"value": 1}),
        )
        .expect("canonical same-origin MCP paths remain valid");
        assert_eq!(
            prepared.endpoint.as_str(),
            "https://mcp.example.test/alternate"
        );

        for (endpoint, expected_error) in [
            (
                format!("https://user:{URL_SECRET}@mcp.example.test/rpc"),
                "userinfo",
            ),
            (
                format!("https://mcp.example.test/rpc?access_token={URL_SECRET}"),
                "query string",
            ),
            (
                format!("https://mcp.example.test/rpc#{URL_SECRET}"),
                "fragment",
            ),
        ] {
            let error = prepare_mcp_request(server, &endpoint, &json!({"value": 1}))
                .expect_err("URL-borne MCP credentials must fail before request preparation");
            let diagnostic = error.to_string();
            assert!(diagnostic.contains(expected_error), "{diagnostic}");
            assert!(!diagnostic.contains(URL_SECRET));
        }

        server.base_url = format!("https://mcp.example.test/rpc?access_token={URL_SECRET}");
        let error = endpoint_candidates(&server.base_url)
            .expect_err("configured MCP query tokens must fail before discovery");
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("query string"));
        assert!(!diagnostic.contains(URL_SECRET));
    }

    #[tokio::test]
    async fn redirect_is_not_followed_with_configured_headers_or_body() {
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect target");
        let target_address = target_listener
            .local_addr()
            .expect("redirect target address");
        let target_requests = Arc::new(AtomicUsize::new(0));
        let observed_target_requests = Arc::clone(&target_requests);
        let target_app = Router::new().fallback(any(move || {
            let observed_target_requests = Arc::clone(&observed_target_requests);
            async move {
                observed_target_requests.fetch_add(1, Ordering::SeqCst);
                StatusCode::NO_CONTENT
            }
        }));
        let target_server = tokio::spawn(async move {
            axum::serve(target_listener, target_app)
                .await
                .expect("serve redirect target");
        });

        let source_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect source");
        let source_address = source_listener
            .local_addr()
            .expect("redirect source address");
        let source_requests = Arc::new(AtomicUsize::new(0));
        let observed_source_requests = Arc::clone(&source_requests);
        let redirect_target = format!("http://{target_address}/capture");
        let source_app = Router::new().route(
            "/mcp",
            post(move || {
                let observed_source_requests = Arc::clone(&observed_source_requests);
                let redirect_target = redirect_target.clone();
                async move {
                    observed_source_requests.fetch_add(1, Ordering::SeqCst);
                    let mut headers = axum::http::HeaderMap::new();
                    headers.insert(
                        axum::http::header::LOCATION,
                        axum::http::HeaderValue::from_str(&redirect_target)
                            .expect("redirect Location"),
                    );
                    (StatusCode::TEMPORARY_REDIRECT, headers, "redirect")
                }
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(source_listener, source_app)
                .await
                .expect("serve redirect source");
        });

        let client = McpClient::new(McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                display_name: None,
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{source_address}/mcp"),
                timeout_ms: 2_000,
                remote_agent: false,
                trusted_certificate_pem: None,
                tool_routes: Vec::new(),
                headers: BTreeMap::from([(
                    "Authorization".to_string(),
                    "Bearer must-not-cross-origin".to_string(),
                )]),
            }],
        });
        let server = &client.config.servers[0];
        let mut checkpoint = || Ok(());
        let error = client
            .post_json(
                server,
                &server.base_url,
                json!({"secret": "body-must-not-cross-origin"}),
                mcp_operation_deadline(server).expect("operation deadline"),
                &mut checkpoint,
            )
            .await
            .expect_err("redirect response must fail closed");

        assert!(error.to_string().contains("HTTP 307"));
        assert_eq!(source_requests.load(Ordering::SeqCst), 1);
        assert_eq!(target_requests.load(Ordering::SeqCst), 0);
        source_server.abort();
        target_server.abort();
    }

    #[tokio::test]
    async fn endpoint_discovery_and_tool_call_share_one_absolute_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind MCP deadline fixture");
        let address = listener.local_addr().expect("fixture address");
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let list_calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let observed_fallback_calls = Arc::clone(&fallback_calls);
        let observed_list_calls = Arc::clone(&list_calls);
        let observed_tool_calls = Arc::clone(&tool_calls);
        let app = Router::new()
            .route(
                "/",
                post(move || {
                    let observed_fallback_calls = Arc::clone(&observed_fallback_calls);
                    async move {
                        observed_fallback_calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        (
                            StatusCode::NOT_FOUND,
                            Json(json!({"error": "not an MCP endpoint"})),
                        )
                    }
                }),
            )
            .route(
                "/mcp",
                post(move |Json(payload): Json<Value>| {
                    let observed_list_calls = Arc::clone(&observed_list_calls);
                    let observed_tool_calls = Arc::clone(&observed_tool_calls);
                    async move {
                        if payload["method"] == "tools/list" {
                            observed_list_calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(150)).await;
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": {"tools": [{"name": "change"}]}
                            }))
                        } else {
                            observed_tool_calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": {"content": [{"type": "text", "text": "ok"}]}
                            }))
                        }
                    }
                }),
            );
        let fixture = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve MCP deadline fixture");
        });

        let client = McpClient::new(McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                display_name: None,
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{address}"),
                timeout_ms: 500,
                remote_agent: false,
                trusted_certificate_pem: None,
                tool_routes: vec![crate::config::McpToolRouteConfig {
                    name: "change".to_string(),
                    effect: ToolEffectClass::Mutation,
                }],
                headers: BTreeMap::new(),
            }],
        });
        let error = client
            .call_tool("fixture", "change", json!({"value": 1}), || Ok(()))
            .await
            .expect_err("discovery and call must consume one deadline");

        assert!(error.to_string().contains("absolute deadline"));
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_eq!(list_calls.load(Ordering::SeqCst), 1);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
        fixture.abort();
    }

    #[tokio::test]
    async fn fallback_endpoint_rechecks_typed_effect_admission_before_second_send() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind MCP fixture");
        let address = listener.local_addr().expect("fixture address");
        let request_count = Arc::new(AtomicUsize::new(0));
        let handler_count = Arc::clone(&request_count);
        let app = Router::new().fallback(any(move || {
            let handler_count = Arc::clone(&handler_count);
            async move {
                handler_count.fetch_add(1, Ordering::SeqCst);
                StatusCode::NOT_FOUND
            }
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve MCP fixture");
        });

        let client = McpClient::new(McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                display_name: None,
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{address}"),
                timeout_ms: 2_000,
                remote_agent: false,
                trusted_certificate_pem: None,
                tool_routes: Vec::new(),
                headers: BTreeMap::new(),
            }],
        });
        let mut checkpoints = 0;
        let error = client
            .list_tools("fixture", || {
                checkpoints += 1;
                if checkpoints == 1 {
                    Ok(())
                } else {
                    Err(ToolError::RunInterrupted)
                }
            })
            .await
            .expect_err("the typed terminal owner must reject the fallback send");

        assert!(matches!(error, ToolError::RunInterrupted));
        assert_eq!(checkpoints, 2);
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn tool_call_uses_one_resolved_endpoint_and_never_falls_back_after_send() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind MCP fixture");
        let address = listener.local_addr().expect("fixture address");
        let root_tool_calls = Arc::new(AtomicUsize::new(0));
        let mcp_tool_calls = Arc::new(AtomicUsize::new(0));
        let root_calls = Arc::clone(&root_tool_calls);
        let mcp_calls = Arc::clone(&mcp_tool_calls);
        let app = Router::new()
            .route(
                "/",
                post(move |Json(payload): Json<Value>| {
                    let root_calls = Arc::clone(&root_calls);
                    async move {
                        if payload["method"] == "tools/call" {
                            root_calls.fetch_add(1, Ordering::SeqCst);
                        }
                        (
                            StatusCode::NOT_FOUND,
                            Json(json!({"error": "not an MCP endpoint"})),
                        )
                    }
                }),
            )
            .route(
                "/mcp",
                post(move |Json(payload): Json<Value>| {
                    let mcp_calls = Arc::clone(&mcp_calls);
                    async move {
                        if payload["method"] == "tools/list" {
                            (
                                StatusCode::OK,
                                Json(json!({
                                    "jsonrpc": "2.0",
                                    "id": 1,
                                    "result": {"tools": [{"name": "change"}]}
                                })),
                            )
                        } else {
                            mcp_calls.fetch_add(1, Ordering::SeqCst);
                            (
                                StatusCode::BAD_GATEWAY,
                                Json(json!({"error": "ambiguous upstream failure"})),
                            )
                        }
                    }
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve MCP fixture");
        });

        let client = McpClient::new(McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                display_name: None,
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{address}"),
                timeout_ms: 2_000,
                remote_agent: false,
                trusted_certificate_pem: None,
                tool_routes: vec![crate::config::McpToolRouteConfig {
                    name: "change".to_string(),
                    effect: ToolEffectClass::Mutation,
                }],
                headers: BTreeMap::new(),
            }],
        });
        let mut checkpoints = 0;
        let error = client
            .call_tool("fixture", "change", json!({"value": 1}), || {
                checkpoints += 1;
                Ok(())
            })
            .await
            .expect_err("ambiguous tools/call failure must be returned without replay");

        assert!(error.to_string().contains("HTTP 502"));
        assert_eq!(checkpoints, 3, "two read probes plus one tools/call");
        assert_eq!(mcp_tool_calls.load(Ordering::SeqCst), 1);
        assert_eq!(root_tool_calls.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
