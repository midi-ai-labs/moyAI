use std::collections::HashMap;
use std::sync::Arc;
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
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("static no-redirect MCP HTTP client configuration"),
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
        let deadline = mcp_operation_deadline(server)?;

        let (endpoint, response) = self
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
                .resolve_endpoint_with_tools_list(server, deadline, &mut effect_checkpoint)
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
                deadline,
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
        deadline: Instant,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<Value, ToolError> {
        let prepared = prepare_mcp_request(server, endpoint, &payload)?;
        let mut request = self
            .http
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
        })
        .await
        .map_err(|_| mcp_deadline_error(server))?
    }

    async fn resolve_endpoint_with_tools_list(
        &self,
        server: &McpServerConfig,
        deadline: Instant,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<(String, Value), ToolError> {
        let endpoints = endpoint_candidates(&server.base_url)?;
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
                    deadline,
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

#[derive(Debug)]
struct PreparedMcpRequest {
    endpoint: reqwest::Url,
    body: Vec<u8>,
    configured_headers: Vec<(HeaderName, HeaderValue)>,
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
        let value = HeaderValue::from_str(value).map_err(|_| {
            ToolError::Message(format!(
                "mcp request header `{visible_name}` has an invalid value"
            ))
        })?;
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
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{source_address}/mcp"),
                timeout_ms: 2_000,
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
                id: "fixture".to_string(),
                enabled: true,
                transport: crate::config::McpTransportKind::Http,
                base_url: format!("http://{address}"),
                timeout_ms: 500,
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
