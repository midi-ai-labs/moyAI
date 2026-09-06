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
            description: "Inspect tools and their input schemas from a configured MCP server, or call a tool. Configured moyAI remote-agent servers can execute tasks on other devices. Omit tool_name to inspect the server first.",
            input_schema: json!({
                "type": "object",
                "required": ["server_id"],
                "properties": {
                    "server_id": { "type": "string" },
                    "tool_name": { "type": "string" },
                    "arguments": { "type": "object", "additionalProperties": true }
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
        let mut input = serde_json::from_value::<McpCallInput>(raw_arguments)?;
        prepare_remote_task_arguments(
            &ctx.config.mcp,
            &mut input,
            std::env::var("COMPUTERNAME").ok().as_deref(),
            &ctx.session.session.id.to_string(),
            &ctx.run_mutation_fence.turn_id().to_string(),
        )?;
        let summary = mcp_permission_summary(ctx.services.mcp.config(), &input);
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
        let network = ctx
            .services
            .store
            .device_network()
            .filter(|network| network.owns_server(&input.server_id));
        let operation = if let Some(network) = network {
            network
                .execute_operation(
                    ctx.session.session.id,
                    ctx.run_mutation_fence.turn_id(),
                    ctx.run_control.clone(),
                    &input.server_id,
                    input
                        .tool_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|name| !name.is_empty()),
                    input.arguments.unwrap_or_else(|| json!({})),
                    || effect_admission.admit(),
                )
                .await?
        } else {
            match input
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
                        if let Some(schema) = &tool.input_schema {
                            let schema =
                                model_input_schema(&ctx.config.mcp, &server_id, &tool.name, schema);
                            lines.push(format!("  Input schema: {schema}"));
                        }
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
                    title: format!(
                        "「{}」のツール一覧を取得",
                        mcp_server_label(ctx.services.mcp.config(), &server_id)
                    ),
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
                    title: format!(
                        "「{}」のツール「{tool_name}」を実行",
                        mcp_server_label(ctx.services.mcp.config(), &server_id)
                    ),
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

fn configured_remote_agent(config: &crate::config::McpConfig, server_id: &str) -> bool {
    config.enabled
        && config
            .servers
            .iter()
            .any(|server| server.id == server_id && server.enabled && server.remote_agent)
}

fn prepare_remote_task_arguments(
    config: &crate::config::McpConfig,
    input: &mut McpCallInput,
    computer_name: Option<&str>,
    task_id: &str,
    turn_id: &str,
) -> Result<(), ToolError> {
    if input.tool_name.as_deref().map(str::trim) != Some("delegate_task")
        || !configured_remote_agent(config, &input.server_id)
    {
        return Ok(());
    }
    let arguments = input.arguments.get_or_insert_with(|| json!({}));
    let object = arguments
        .as_object_mut()
        .ok_or_else(|| ToolError::Message("remote task arguments must be an object".into()))?;
    // Receiver protocol tokens are ASCII and bounded. The label is provenance,
    // not authentication; a localized hostname must not prevent delegation.
    let peer_id = computer_name
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        })
        .unwrap_or("moyAI");
    // The paired profile owns receiver authority; the model cannot choose the
    // local task/turn recorded as the parent of this delegation.
    object.insert(
        "parent".into(),
        json!({ "peer_id": peer_id, "task_id": task_id, "turn_id": turn_id }),
    );
    Ok(())
}

fn model_input_schema(
    config: &crate::config::McpConfig,
    server_id: &str,
    tool_name: &str,
    wire_schema: &Value,
) -> Value {
    let mut schema = wire_schema.clone();
    if tool_name == "delegate_task" && configured_remote_agent(config, server_id) {
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            properties.remove("parent");
        }
        if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|field| field.as_str() != Some("parent"));
        }
    }
    schema
}

fn mcp_server_label<'a>(config: &'a crate::config::McpConfig, server_id: &'a str) -> &'a str {
    config
        .servers
        .iter()
        .find(|server| server.id == server_id)
        .and_then(|server| server.display_name.as_deref())
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or(server_id)
}

fn mcp_permission_summary(config: &crate::config::McpConfig, input: &McpCallInput) -> String {
    let label = mcp_server_label(config, &input.server_id);
    match input
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(tool_name) => format!("「{label}」のツール「{tool_name}」を実行"),
        None => format!("「{label}」の利用できるツールを確認"),
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
    let mut details = vec![format!(
        "接続先: {}\n接続先ID: {}\n設定URL: {target}",
        mcp_server_label(config, &input.server_id),
        input.server_id
    )];
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

    use super::{
        mcp_permission_material, mcp_permission_summary, model_input_schema,
        prepare_remote_task_arguments, redact_mcp_target, redact_mcp_value,
    };
    use crate::config::{McpConfig, McpServerConfig, McpTransportKind};
    use crate::tool::permission_guardian::{
        PermissionGuardianEvidence, PermissionGuardianEvidenceState,
    };
    use crate::tool::registry::Tool;

    fn config() -> McpConfig {
        McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                display_name: None,
                id: "fixture".to_string(),
                enabled: true,
                transport: McpTransportKind::Http,
                base_url: "https://mcp.example.test/rpc".to_string(),
                timeout_ms: 1_000,
                remote_agent: false,
                trusted_certificate_pem: None,
                tool_routes: Vec::new(),
                headers: Default::default(),
            }],
        }
    }

    #[test]
    fn named_mcp_permission_keeps_exact_target_and_guardian_evidence() {
        let mut config = config();
        let id = format!("hub-device-{}", "a".repeat(64));
        config.servers[0].id = id.clone();
        config.servers[0]
            .headers
            .insert("Authorization".into(), "Bearer hidden-token".into());
        for tool_name in [None, Some("delegate_task")] {
            let input = super::McpCallInput {
                server_id: id.clone(),
                tool_name: tool_name.map(str::to_string),
                arguments: tool_name.map(|_| json!({"prompt":"CPU情報を確認"})),
            };
            let (_, original) = mcp_permission_material(&config, &input);
            config.servers[0].display_name = Some("Win20-Worker (端末20)".into());
            let summary = mcp_permission_summary(&config, &input);
            assert!(summary.contains("Win20-Worker"));
            assert!(!summary.contains(&id));
            let (details, named) = mcp_permission_material(&config, &input);
            assert!(details[0].contains("Win20-Worker"));
            assert!(details[0].contains(&id));
            assert!(details[0].contains(&config.servers[0].base_url));
            assert!(!details.join("\n").contains("hidden-token"));
            let (
                PermissionGuardianEvidenceState::Complete(original),
                PermissionGuardianEvidenceState::Complete(named),
            ) = (original, named)
            else {
                panic!("expected complete evidence for the same configured endpoint");
            };
            assert_eq!(
                original, named,
                "a public label never changes authority or effect evidence"
            );
            assert_eq!(input.server_id, id);
            config.servers[0].display_name = None;
        }
    }

    #[test]
    fn mcp_permission_without_a_public_label_uses_its_stable_id() {
        let mut config = config();
        let input = super::McpCallInput {
            server_id: "fixture".into(),
            tool_name: None,
            arguments: None,
        };
        for label in [None, Some(" ")] {
            config.servers[0].display_name = label.map(str::to_string);
            assert!(mcp_permission_summary(&config, &input).contains("fixture"));
        }
        config.servers[0].display_name = Some("別の接続先".into());
        let unknown = super::McpCallInput {
            server_id: "unconfigured".into(),
            ..input
        };
        assert!(mcp_permission_summary(&config, &unknown).contains("unconfigured"));
        assert!(!mcp_permission_summary(&config, &unknown).contains("別の接続先"));
    }

    #[test]
    fn mcp_call_advertises_object_arguments_while_allowing_server_specific_fields() {
        let schema = super::McpCallTool.spec().input_schema;
        assert_eq!(schema["properties"]["arguments"]["type"], "object");
        assert_eq!(
            schema["properties"]["arguments"]["additionalProperties"],
            true
        );
        assert_eq!(
            schema["required"],
            json!(["server_id"]),
            "tools/list does not require arguments or a tool name"
        );
    }

    #[test]
    fn remote_delegation_overwrites_model_parent_with_the_current_task_and_turn() {
        let mut config = config();
        config.servers[0].remote_agent = true;
        for tool_name in ["delegate_task", " delegate_task "] {
            let mut input = super::McpCallInput {
                server_id: "fixture".into(),
                tool_name: Some(tool_name.into()),
                arguments: Some(json!({
                    "request_key": "retry-key",
                    "prompt": "Investigate this project",
                    "parent": {"peer_id":"fabricated","task_id":"wrong-task","turn_id":"wrong-turn"},
                })),
            };
            prepare_remote_task_arguments(
                &config,
                &mut input,
                Some("WinA-01"),
                "canonical-task",
                "canonical-turn",
            )
            .expect("prepare remote delegation");
            let arguments = input.arguments.expect("arguments");
            assert_eq!(
                arguments["parent"],
                json!({
                    "peer_id":"WinA-01", "task_id":"canonical-task", "turn_id":"canonical-turn",
                })
            );
            assert_eq!(arguments["request_key"], "retry-key");
            assert_eq!(arguments["prompt"], "Investigate this project");
        }
    }

    #[test]
    fn remote_delegation_uses_a_protocol_safe_label_for_unicode_or_invalid_hostnames() {
        let mut config = config();
        config.servers[0].remote_agent = true;
        let oversized = "a".repeat(129);
        for computer_name in [
            None,
            Some(""),
            Some("日本語端末"),
            Some("Office PC"),
            Some(oversized.as_str()),
        ] {
            let mut input = super::McpCallInput {
                server_id: "fixture".into(),
                tool_name: Some("delegate_task".into()),
                arguments: Some(json!({"request_key":"key", "prompt":"task"})),
            };
            prepare_remote_task_arguments(&config, &mut input, computer_name, "task-id", "turn-id")
                .expect("prepare localized host delegation");
            assert_eq!(
                input.arguments.as_ref().expect("arguments")["parent"],
                json!({
                    "peer_id":"moyAI", "task_id":"task-id", "turn_id":"turn-id",
                })
            );
            let request: crate::remote_agent::RemoteTaskRequest =
                serde_json::from_value(input.arguments.expect("arguments"))
                    .expect("receiver request shape");
            assert!(
                request.validate(),
                "receiver accepts the normalized provenance"
            );
        }
    }

    #[test]
    fn remote_model_schema_hides_injected_parent_without_changing_external_wire_schema() {
        let wire_schema = json!({
            "type":"object", "additionalProperties":false,
            "properties":{
                "request_key":{"type":"string","maxLength":128},
                "prompt":{"type":"string","maxLength":32768},
                "parent":{"type":"object","required":["peer_id","task_id","turn_id"]},
            },
            "required":["request_key","parent","prompt"],
        });
        let mut config = config();
        assert_eq!(
            model_input_schema(&config, "fixture", "delegate_task", &wire_schema),
            wire_schema,
            "an ordinary MCP server may define its own parent argument"
        );
        config.servers[0].remote_agent = true;
        let visible = model_input_schema(&config, "fixture", "delegate_task", &wire_schema);
        assert_eq!(visible["required"], json!(["request_key", "prompt"]));
        assert_eq!(
            visible["properties"],
            json!({
                "request_key":{"type":"string","maxLength":128},
                "prompt":{"type":"string","maxLength":32768},
            })
        );
        assert_eq!(visible["additionalProperties"], false);
        assert!(
            wire_schema["properties"].get("parent").is_some(),
            "the external descriptor is immutable"
        );
        assert_eq!(
            model_input_schema(&config, "fixture", "task_status", &wire_schema),
            wire_schema
        );
        assert_eq!(
            model_input_schema(&config, "other-server", "delegate_task", &wire_schema),
            wire_schema
        );
        config.servers[0].enabled = false;
        assert_eq!(
            model_input_schema(&config, "fixture", "delegate_task", &wire_schema),
            wire_schema
        );
    }

    #[test]
    fn generic_mcp_arguments_are_unchanged_and_remote_non_object_arguments_are_rejected() {
        let mut config = config();
        let original = json!({"parent":{"external":"authority"},"custom":"value"});
        let mut input = super::McpCallInput {
            server_id: "fixture".into(),
            tool_name: Some("delegate_task".into()),
            arguments: Some(original.clone()),
        };
        prepare_remote_task_arguments(&config, &mut input, Some("WinA"), "task", "turn")
            .expect("generic MCP arguments");
        assert_eq!(input.arguments, Some(original));
        config.servers[0].remote_agent = true;
        for invalid in [
            json!(["invalid"]),
            json!(r#"{"request_key":"key","prompt":"task"}"#),
        ] {
            input.arguments = Some(invalid.clone());
            assert!(
                prepare_remote_task_arguments(&config, &mut input, Some("WinA"), "task", "turn")
                    .is_err()
            );
            assert_eq!(
                input.arguments,
                Some(invalid),
                "stringified JSON is not reinterpreted"
            );
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
