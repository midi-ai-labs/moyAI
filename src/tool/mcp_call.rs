use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
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
        let effect_admission = ctx
            .confirm_if_needed(
                effect.access_kind(),
                summary,
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
                let output_text = if tools.is_empty() {
                    format!(
                        "MCP server `{server_id}` returned no tools. Check the server configuration or explicit tool routes before retrying."
                    )
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
                            "[{} additional tools omitted by output limit]",
                            tools.len() - visible_tools.len()
                        ));
                    }
                    lines.join("\n")
                };
                let truncated = ctx.services.truncator.preview(
                    output_text,
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
                        "tools": visible_tools.iter().map(|tool| json!({
                            "name": tool.name.clone(),
                            "effect": tool.effect,
                            "description": tool.description.clone(),
                            "annotations": tool.annotations.clone(),
                            "input_schema": tool.input_schema.clone(),
                        })).collect::<Vec<_>>(),
                    }),
                    truncated_output_path: truncated.truncated_output_path,
                    recorded_changes: Vec::new(),
                    change_summaries: Vec::new(),
                })
            }
            crate::mcp::McpOperationResult::ToolCalled {
                server_id,
                endpoint,
                tool_name,
                output_text,
                raw_result,
            } => {
                let truncated = ctx.services.truncator.preview(
                    output_text,
                    &ctx.config.tool_output,
                    &ctx.services.storage_paths,
                )?;
                let raw_json = serde_json::to_string(&raw_result)?;
                let raw_result_preview = crate::tool::truncate::clip_text_with_ellipsis(
                    &raw_json,
                    ctx.config.tool_output.max_bytes,
                );
                Ok(ToolResult {
                    title: format!("Called MCP tool {server_id}:{tool_name}"),
                    output_text: truncated.preview_text,
                    metadata: json!({
                        "server_id": server_id,
                        "endpoint": endpoint,
                        "tool_name": tool_name,
                        "effect": effect,
                        "raw_result_bytes": raw_json.len(),
                        "raw_result_sha256": crate::harness::artifact::hash_bytes(raw_json.as_bytes()),
                        "raw_result_preview": raw_result_preview,
                    }),
                    truncated_output_path: truncated.truncated_output_path,
                    recorded_changes: Vec::new(),
                    change_summaries: Vec::new(),
                })
            }
        }
    }
}
