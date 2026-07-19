use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::permission_guardian::{
    PermissionGuardianEvidence, PermissionGuardianEvidenceState,
};
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
        let (details, guardian_evidence) =
            mcp_permission_material(ctx.services.mcp.config(), &input);
        let effect_admission = ctx
            .confirm_if_needed_with_details_and_guardian_evidence(
                effect.access_kind(),
                summary,
                details,
                Vec::new(),
                false,
                effect.permission_risks(),
                guardian_evidence,
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

fn mcp_permission_material(
    config: &crate::config::McpConfig,
    input: &McpCallInput,
) -> (Vec<String>, PermissionGuardianEvidenceState) {
    let configured_server = config
        .servers
        .iter()
        .find(|server| server.id == input.server_id);
    let server = configured_server.filter(|server| config.enabled && server.enabled);
    let (target, target_was_redacted) = configured_server
        .map(|server| redact_mcp_target_with_status(&server.base_url))
        .unwrap_or_else(|| ("[unconfigured server]".to_string(), true));
    let mut details = vec![format!("Configured target: {target}")];
    let tool_name = input
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let (arguments, arguments_were_redacted) = if tool_name.is_some() {
        let raw_arguments = input
            .arguments
            .as_ref()
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let (visible_arguments, arguments_were_redacted) =
            redact_mcp_value_with_status(&raw_arguments);
        let rendered = serde_json::to_string_pretty(&visible_arguments)
            .unwrap_or_else(|_| "[arguments could not be rendered]".to_string());
        details.push(format!(
            "Arguments:\n{}",
            clip_text_with_ellipsis(&rendered, 2_000)
        ));
        (raw_arguments, arguments_were_redacted)
    } else {
        (Value::Object(Default::default()), false)
    };

    let guardian_evidence = match (server, target_was_redacted, arguments_were_redacted) {
        (None, _, _) => PermissionGuardianEvidenceState::incomplete(
            "the requested MCP server is not configured and enabled",
        ),
        (Some(_), true, _) => PermissionGuardianEvidenceState::incomplete(
            "the configured MCP target contains redacted or invalid URL components",
        ),
        (Some(_), _, true) => PermissionGuardianEvidenceState::incomplete(
            "the MCP arguments contain sensitive values that cannot be disclosed to the guardian",
        ),
        (Some(_), false, false) => {
            let credential_present = server.is_some_and(|server| !server.headers.is_empty());
            let evidence = match tool_name {
                Some(tool_name) => PermissionGuardianEvidence::McpCall {
                    server_id: input.server_id.clone(),
                    configured_target: target,
                    credential_present,
                    tool_name: tool_name.to_string(),
                    arguments,
                },
                None => PermissionGuardianEvidence::McpListTools {
                    server_id: input.server_id.clone(),
                    configured_target: target,
                    credential_present,
                },
            };
            PermissionGuardianEvidenceState::Complete(evidence)
        }
    };
    (details, guardian_evidence)
}

#[cfg(test)]
fn redact_mcp_target(value: &str) -> String {
    redact_mcp_target_with_status(value).0
}

fn redact_mcp_target_with_status(value: &str) -> (String, bool) {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return ("[configured target is not a valid URL]".to_string(), true);
    };
    let mut redacted = false;
    if !url.username().is_empty() {
        let _ = url.set_username("[redacted]");
        redacted = true;
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("[redacted]"));
        redacted = true;
    }
    if url.query().is_some() {
        redacted = true;
    }
    if url.fragment().is_some() {
        redacted = true;
    }
    url.set_query(None);
    url.set_fragment(None);
    (url.to_string(), redacted)
}

#[cfg(test)]
fn redact_mcp_value(value: &Value) -> Value {
    redact_mcp_value_with_status(value).0
}

fn redact_mcp_value_with_status(value: &Value) -> (Value, bool) {
    match value {
        Value::Object(object) => {
            let mut redacted = false;
            let visible = object
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
                    let visible_value = if sensitive {
                        redacted = true;
                        Value::String("[redacted]".to_string())
                    } else {
                        let (visible, nested_redaction) = redact_mcp_value_with_status(value);
                        redacted |= nested_redaction;
                        visible
                    };
                    (key.clone(), visible_value)
                })
                .collect();
            (Value::Object(visible), redacted)
        }
        Value::Array(values) => {
            let mut redacted = false;
            let visible = values
                .iter()
                .map(|value| {
                    let (visible, nested_redaction) = redact_mcp_value_with_status(value);
                    redacted |= nested_redaction;
                    visible
                })
                .collect();
            (Value::Array(visible), redacted)
        }
        _ => (value.clone(), false),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{mcp_permission_material, redact_mcp_target, redact_mcp_value};
    use crate::config::{McpConfig, McpServerConfig, McpTransportKind};
    use crate::tool::permission_guardian::{
        PermissionGuardianEvidence, PermissionGuardianEvidenceState,
    };

    fn config() -> McpConfig {
        McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                id: "fixture".to_string(),
                enabled: true,
                transport: McpTransportKind::Http,
                base_url: "https://mcp.example.test/rpc".to_string(),
                timeout_ms: 1_000,
                tool_routes: Vec::new(),
                headers: Default::default(),
            }],
        }
    }

    fn assert_tail_evidence(operation: &str) {
        let input = super::McpCallInput {
            server_id: "fixture".to_string(),
            tool_name: Some("documents.update".to_string()),
            arguments: Some(json!({
                "a_padding": "x".repeat(2_500),
                "z_operation": operation,
            })),
        };
        let (details, evidence) = mcp_permission_material(&config(), &input);

        assert!(details[1].len() < 2_100);
        assert!(!details[1].contains(operation));
        let PermissionGuardianEvidenceState::Complete(PermissionGuardianEvidence::McpCall {
            arguments,
            ..
        }) = evidence
        else {
            panic!("expected complete MCP call evidence");
        };
        assert_eq!(arguments["z_operation"], operation);
        assert_eq!(arguments["a_padding"].as_str().map(str::len), Some(2_500));
    }

    #[test]
    fn guardian_evidence_preserves_positive_decisive_field_beyond_human_preview() {
        assert_tail_evidence("read_only");
    }

    #[test]
    fn guardian_evidence_preserves_negative_decisive_field_beyond_human_preview() {
        assert_tail_evidence("delete_everything");
    }

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

    #[test]
    fn sensitive_arguments_are_human_redacted_and_not_auto_reviewable() {
        let input = super::McpCallInput {
            server_id: "fixture".to_string(),
            tool_name: Some("documents.update".to_string()),
            arguments: Some(json!({"document_id": "doc-42", "api_token": "secret-value"})),
        };
        let (details, evidence) = mcp_permission_material(&config(), &input);

        assert!(details[1].contains("[redacted]"));
        assert!(!details[1].contains("secret-value"));
        assert!(matches!(
            evidence,
            PermissionGuardianEvidenceState::Incomplete { .. }
        ));
    }

    #[test]
    fn mcp_evidence_reports_credentials_without_disclosing_header_values() {
        let mut config = config();
        config.servers[0].headers.insert(
            "Authorization".to_string(),
            "Bearer header-secret".to_string(),
        );
        let input = super::McpCallInput {
            server_id: "fixture".to_string(),
            tool_name: Some("documents.read".to_string()),
            arguments: Some(json!({"document_id": "doc-42"})),
        };
        let (_, evidence) = mcp_permission_material(&config, &input);
        let PermissionGuardianEvidenceState::Complete(evidence) = evidence else {
            panic!("expected complete MCP evidence");
        };
        let payload = serde_json::to_value(evidence).expect("serialize evidence");

        assert_eq!(payload["credential_present"], true);
        let serialized = payload.to_string();
        assert!(!serialized.contains("header-secret"));
        assert!(!serialized.contains("Authorization"));
    }
}
