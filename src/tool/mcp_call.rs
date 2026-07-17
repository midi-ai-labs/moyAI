use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{ToolName, ToolResult, ToolSpec};

#[derive(Debug, Deserialize)]
pub struct McpCallInput {
    pub server_id: String,
    pub tool_name: Option<String>,
    pub arguments: Option<Value>,
}

#[derive(Debug, Default)]
pub struct McpCallTool;

#[async_trait(?Send)]
impl Tool for McpCallTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::McpCall,
            effect: crate::tool::ToolEffectPolicy::McpCall,
            description: "List tools from a configured MCP server, or call a specific MCP tool. Use this for explicit MCP workflows that are configured in the current environment.",
            input_schema: json!({
                "type": "object",
                "required": ["server_id"],
                "properties": {
                    "server_id": { "type": "string" },
                    "tool_name": { "type": "string" },
                    "arguments": {}
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let effect =
            crate::tool::ToolEffectPolicy::McpCall.resolve(&raw_arguments, &ctx.config.mcp);
        let input = serde_json::from_value::<McpCallInput>(raw_arguments)?;
        let summary = match input
            .tool_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            Some(tool_name) => format!("Call MCP tool {}:{}", input.server_id, tool_name),
            None => format!("List MCP tools from {}", input.server_id),
        };
        let details = mcp_permission_details(ctx.services.mcp.config(), &input);
        let effect_admission = ctx
            .confirm_if_needed_with_details(
                effect.access_kind(),
                summary,
                details,
                Vec::new(),
                false,
                effect.permission_risks(),
            )
            .await?;
        let operation = match input
            .tool_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            Some(tool_name) => {
                ctx.services
                    .mcp
                    .call_tool(
                        &input.server_id,
                        tool_name,
                        input.arguments.unwrap_or(Value::Object(Default::default())),
                        || effect_admission.admit(),
                    )
                    .await?
            }
            None => {
                ctx.services
                    .mcp
                    .list_tools(&input.server_id, || effect_admission.admit())
                    .await?
            }
        };

        match operation {
            crate::mcp::McpOperationResult::ToolsListed {
                server_id,
                endpoint,
                tools,
            } => {
                let visible_tools = tools
                    .iter()
                    .take(ctx.config.tool_output.max_results)
                    .collect::<Vec<_>>();
                let output_chunks = if tools.is_empty() {
                    vec![format!("MCP server `{server_id}` returned 0 tools.")]
                } else {
                    let mut lines = vec![format!("MCP tools for `{server_id}`:")];
                    for tool in &visible_tools {
                        let description = tool.description.as_deref().unwrap_or("no description");
                        lines.push(format!(
                            "- {} [{}]: {}",
                            tool.name, tool.effect, description
                        ));
                    }
                    if visible_tools.len() < tools.len() {
                        lines.push(format!(
                            "[{} tools omitted by output limit]",
                            tools.len() - visible_tools.len()
                        ));
                    }
                    lines
                };
                let truncated = ctx.services.truncator.preview_chunks(
                    output_chunks,
                    "\n",
                    &ctx.config.tool_output,
                    &ctx.services.storage_paths,
                )?;
                Ok(ToolResult {
                    title: format!("Listed MCP tools from {server_id}"),
                    output_text: truncated.preview_text,
                    metadata: json!({
                        "server_id": server_id,
                        "endpoint": endpoint,
                        "tool_count": tools.len(),
                        "omitted_tool_count": tools.len().saturating_sub(visible_tools.len()),
                        "tools": visible_tools.iter().map(|tool| json!({
                            "name": tool.name.clone(),
                            "effect": tool.effect,
                        })).collect::<Vec<_>>(),
                        "truncated": truncated.truncated,
                    }),
                    truncated_output_path: truncated.truncated_output_path,
                    recorded_changes: Vec::new(),
                    change_summaries: Vec::new(),
                    _internal_file_lease: truncated.internal_file_lease,
                })
            }
            crate::mcp::McpOperationResult::ToolCalled {
                server_id,
                endpoint,
                tool_name,
                output_text,
                raw_result: _,
            } => {
                let truncated = ctx.services.truncator.preview(
                    output_text,
                    &ctx.config.tool_output,
                    &ctx.services.storage_paths,
                )?;
                Ok(ToolResult {
                    title: format!("Called MCP tool {server_id}:{tool_name}"),
                    output_text: truncated.preview_text,
                    metadata: json!({
                        "server_id": server_id,
                        "endpoint": endpoint,
                        "tool_name": tool_name,
                        "effect": effect,
                        "truncated": truncated.truncated,
                    }),
                    truncated_output_path: truncated.truncated_output_path,
                    recorded_changes: Vec::new(),
                    change_summaries: Vec::new(),
                    _internal_file_lease: truncated.internal_file_lease,
                })
            }
        }
    }
}

fn mcp_permission_details(config: &crate::config::McpConfig, input: &McpCallInput) -> Vec<String> {
    let target = config
        .servers
        .iter()
        .find(|server| server.id == input.server_id)
        .map(|server| redact_mcp_target(&server.base_url))
        .unwrap_or_else(|| "[unconfigured server]".to_string());
    let mut details = vec![format!("Configured target: {target}")];
    if input
        .tool_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
    {
        let arguments = input
            .arguments
            .as_ref()
            .map(redact_mcp_value)
            .unwrap_or_else(|| Value::Object(Default::default()));
        let rendered = serde_json::to_string_pretty(&arguments)
            .unwrap_or_else(|_| "[arguments could not be rendered]".to_string());
        details.push(format!(
            "Arguments:\n{}",
            clip_text_with_ellipsis(&rendered, 2_000)
        ));
    }
    details
}

fn redact_mcp_target(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return "[configured target is not a valid URL]".to_string();
    };
    if !url.username().is_empty() {
        let _ = url.set_username("[redacted]");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("[redacted]"));
    }
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn redact_mcp_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    let sensitive = [
                        "authorization",
                        "credential",
                        "password",
                        "secret",
                        "api_key",
                        "apikey",
                        "token",
                    ]
                    .iter()
                    .any(|marker| normalized.contains(marker));
                    (
                        key.clone(),
                        if sensitive {
                            Value::String("[redacted]".to_string())
                        } else {
                            redact_mcp_value(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_mcp_value).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{redact_mcp_target, redact_mcp_value};

    #[test]
    fn permission_arguments_preserve_targets_and_redact_secrets() {
        let redacted = redact_mcp_value(&json!({
            "target": {"document_id": "doc-42"},
            "api_token": "secret-value",
            "nested": [{"password": "also-secret", "operation": "delete"}]
        }));

        assert_eq!(redacted["target"]["document_id"], "doc-42");
        assert_eq!(redacted["api_token"], "[redacted]");
        assert_eq!(redacted["nested"][0]["password"], "[redacted]");
        assert_eq!(redacted["nested"][0]["operation"], "delete");
    }

    #[test]
    fn permission_target_omits_credentials_query_and_fragment() {
        let target = redact_mcp_target(
            "https://user:password@example.test/mcp?access_token=secret#fragment",
        );

        assert!(target.contains("example.test/mcp"));
        assert!(!target.contains("password"));
        assert!(!target.contains("secret"));
        assert!(!target.contains("fragment"));
    }
}
