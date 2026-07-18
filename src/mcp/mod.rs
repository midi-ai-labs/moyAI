use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{McpConfig, McpServerConfig};
use crate::error::ToolError;
use crate::tool::ToolEffectClass;
use crate::tool::truncate::clip_text_with_ellipsis;

pub const MCP_TOOLS_LIST_DESCRIPTOR_SCHEMA_VALIDATION_MARKER: &str =
    "mcp_tools_list_descriptor_schema_validation";
const MAX_MCP_RESPONSE_BYTES: usize = 1024 * 1024;

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

#[derive(Debug, Clone)]
pub struct McpClient {
    config: McpConfig,
    http: reqwest::Client,
    resolved_endpoints: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
}

impl McpClient {
    pub fn new(config: McpConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            resolved_endpoints: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    pub fn config(&self) -> &McpConfig {
        &self.config
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

        let (endpoint, response) = self
            .resolve_endpoint_with_tools_list(server, &mut effect_checkpoint)
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
            endpoint,
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

        let endpoint = if let Some(endpoint) = self
            .resolved_endpoints
            .lock()
            .await
            .get(&server.id)
            .cloned()
        {
            endpoint
        } else {
            let (endpoint, response) = self
                .resolve_endpoint_with_tools_list(server, &mut effect_checkpoint)
                .await?;
            let tools = parse_tools(&response)?;
            if !tools.iter().any(|tool| tool.name == tool_name) {
                return Err(ToolError::Message(format!(
                    "mcp server `{}` did not advertise tool `{tool_name}` during endpoint resolution",
                    server.id
                )));
            }
            endpoint
        };
        // The effectful request has exactly one transport boundary. Endpoint
        // discovery is completed by a read-only tools/list call before this
        // point, so an ambiguous HTTP failure can never replay tools/call at a
        // fallback URL.
        let response = self
            .post_json(
                server,
                &endpoint,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": tool_name,
                        "arguments": arguments,
                    }
                }),
                &mut effect_checkpoint,
            )
            .await?;
        if let Some(error) = response.get("error") {
            return Err(ToolError::Message(format!(
                "mcp tools/call returned an error: {}",
                serde_json::to_string(error).unwrap_or_else(|_| error.to_string())
            )));
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| ToolError::Message("mcp response is missing `result`".to_string()))?;
        Ok(McpOperationResult::ToolCalled {
            server_id: server.id.clone(),
            endpoint,
            tool_name: tool_name.to_string(),
            output_text: render_tool_call_output(&result),
            raw_result: result,
        })
    }

    fn server(&self, server_id: &str) -> Result<&McpServerConfig, ToolError> {
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

    async fn post_json(
        &self,
        server: &McpServerConfig,
        endpoint: &str,
        payload: Value,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<Value, ToolError> {
        let mut request = self
            .http
            .post(endpoint)
            .timeout(Duration::from_millis(server.timeout_ms))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        for (name, value) in &server.headers {
            request = request.header(name, value);
        }

        let request = request.body(payload.to_string());
        // Re-check the typed run owner at every actual send boundary. Endpoint
        // discovery may issue multiple read-only tools/list requests, while a
        // tools/call request reaches this boundary exactly once.
        effect_checkpoint()?;
        let response = request
            .send()
            .await
            .map_err(|error| ToolError::Message(format!("mcp request failed: {error}")))?;
        let status = response.status();
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
            append_bounded_response_chunk(&mut body_bytes, &chunk, endpoint)?;
        }
        let body = String::from_utf8(body_bytes)
            .map_err(|_| ToolError::Message("mcp response body is not valid UTF-8".to_string()))?;
        if !status.is_success() {
            let mut hint = String::new();
            if body.to_ascii_lowercase().contains("invalid host header") {
                hint = " Configure `[mcp.servers[].headers]` if this server requires a specific Host header.".to_string();
            }
            return Err(ToolError::Message(format!(
                "mcp request to `{endpoint}` failed with HTTP {}: {}.{}",
                status.as_u16(),
                compact_body(&body),
                hint
            )));
        }
        parse_json_or_sse(&body)
    }

    async fn resolve_endpoint_with_tools_list(
        &self,
        server: &McpServerConfig,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<(String, Value), ToolError> {
        let endpoints = endpoint_candidates(&server.base_url);
        if endpoints.is_empty() {
            return Err(ToolError::Message(
                "mcp endpoint is not configured".to_string(),
            ));
        }
        let mut last_error = None;
        for endpoint in endpoints {
            match self
                .post_json(
                    server,
                    &endpoint,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/list",
                        "params": {}
                    }),
                    effect_checkpoint,
                )
                .await
            {
                Ok(response) => {
                    parse_tools(&response)?;
                    self.resolved_endpoints
                        .lock()
                        .await
                        .insert(server.id.clone(), endpoint.clone());
                    return Ok((endpoint, response));
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            ToolError::Message("mcp request failed without a detailed error".to_string())
        }))
    }
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

fn endpoint_candidates(base_url: &str) -> Vec<String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.ends_with("/mcp") {
        vec![trimmed.to_string()]
    } else {
        vec![trimmed.to_string(), format!("{trimmed}/mcp")]
    }
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

    Err(ToolError::Message(format!(
        "failed to parse mcp response body: {}",
        compact_body(body)
    )))
}

fn parse_tools(response: &Value) -> Result<Vec<McpToolDescriptor>, ToolError> {
    if let Some(error) = response.get("error") {
        return Err(ToolError::Message(format!(
            "mcp tools/list returned an error: {}",
            serde_json::to_string(error).unwrap_or_else(|_| error.to_string())
        )));
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
                "mcp tools/list descriptor `{name}` has non-string `description`"
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
            Some(value) => Some(serde_json::from_value(value.clone()).map_err(|error| {
                ToolError::Message(format!(
                    "mcp tools/list descriptor `{name}` has invalid `annotations`: {error}"
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

fn compact_body(body: &str) -> String {
    let single_line = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if single_line.len() <= 240 {
        single_line
    } else {
        clip_text_with_ellipsis(&single_line, 243)
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
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: "http://mcp.invalid".to_string(),
                timeout_ms: 2_000,
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
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{address}"),
                timeout_ms: 2_000,
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
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{address}"),
                timeout_ms: 2_000,
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
