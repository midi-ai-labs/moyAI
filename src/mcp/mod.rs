use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{McpConfig, McpServerConfig};
use crate::error::ToolError;
use crate::tool::truncate::clip_text_with_ellipsis;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
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
}

impl McpClient {
    pub fn new(config: McpConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    pub async fn list_tools(
        &self,
        server_id: &str,
        route: &str,
    ) -> Result<McpOperationResult, ToolError> {
        if !self.config.enabled {
            return Err(ToolError::Message("mcp is disabled by config".to_string()));
        }
        let server = self.server(server_id)?;
        self.ensure_route_allowed(server, route)?;

        let (endpoint, response) = self
            .post_json_with_fallback(
                server,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/list",
                    "params": {}
                }),
            )
            .await?;
        let tools = parse_tools(&response)?;
        let filtered = if server.tool_allowlist.is_empty() {
            tools
        } else {
            tools
                .into_iter()
                .filter(|tool| server.tool_allowlist.iter().any(|name| name == &tool.name))
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
        route: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<McpOperationResult, ToolError> {
        if !self.config.enabled {
            return Err(ToolError::Message("mcp is disabled by config".to_string()));
        }
        let server = self.server(server_id)?;
        self.ensure_route_allowed(server, route)?;
        if !server.tool_allowlist.is_empty()
            && !server.tool_allowlist.iter().any(|name| name == tool_name)
        {
            return Err(ToolError::Message(format!(
                "mcp server `{}` does not allow tool `{tool_name}` in current config",
                server.id
            )));
        }

        let (endpoint, response) = self
            .post_json_with_fallback(
                server,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": tool_name,
                        "arguments": arguments,
                    }
                }),
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

    fn ensure_route_allowed(&self, server: &McpServerConfig, route: &str) -> Result<(), ToolError> {
        if server.route_allowlist.is_empty()
            || server
                .route_allowlist
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(route))
        {
            return Ok(());
        }
        Err(ToolError::Message(format!(
            "mcp server `{}` is not enabled for route `{route}`",
            server.id
        )))
    }

    async fn post_json(
        &self,
        server: &McpServerConfig,
        endpoint: &str,
        payload: Value,
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

        let response = request
            .body(payload.to_string())
            .send()
            .await
            .map_err(|error| ToolError::Message(format!("mcp request failed: {error}")))?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            ToolError::Message(format!("failed to read mcp response body: {error}"))
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
    }

    async fn post_json_with_fallback(
        &self,
        server: &McpServerConfig,
        payload: Value,
    ) -> Result<(String, Value), ToolError> {
        let endpoints = endpoint_candidates(&server.base_url);
        if endpoints.is_empty() {
            return Err(ToolError::Message(
                "mcp endpoint is not configured".to_string(),
            ));
        }
        let mut last_error = None;
        for endpoint in endpoints {
            match self.post_json(server, &endpoint, payload.clone()).await {
                Ok(response) => return Ok((endpoint, response)),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            ToolError::Message("mcp request failed without a detailed error".to_string())
        }))
    }
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
    Ok(tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?.to_string();
            Some(McpToolDescriptor {
                name,
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_schema: tool.get("inputSchema").cloned(),
            })
        })
        .collect())
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
