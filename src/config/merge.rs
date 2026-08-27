use super::model::{
    PartialDoclingConfig, PartialFileGuardConfig, PartialFormatConfig, PartialInspectionConfig,
    PartialInstructionConfig, PartialLoggingConfig, PartialMcpConfig, PartialModelConfig,
    PartialMultiAgentConfig, PartialPermissionsConfig, PartialResolvedConfig, PartialSessionConfig,
    PartialShellConfig, PartialToolOutputConfig, PartialWorkspaceConfig, ResolvedConfig,
};
use super::turn::ProviderEndpoint;

pub(crate) fn normalize_provider_profile_alias(
    patch: &mut PartialResolvedConfig,
    inherited_profile: crate::config::ProviderProfile,
    canonical_name: &str,
    legacy_metadata_name: &str,
    legacy_api_name: &str,
) -> Result<(), String> {
    let Some(model) = patch.model.as_mut() else {
        return Ok(());
    };
    let canonical = model.provider_profile;
    let legacy_metadata = model.provider_metadata_mode;
    let legacy_api = model.provider_api_mode;

    if let Some(canonical) = canonical {
        if let Some(legacy_metadata) = legacy_metadata
            && legacy_metadata != canonical.metadata_mode()
        {
            return Err(format!(
                "`{canonical_name}` conflicts with legacy `{legacy_metadata_name}`; remove the legacy field or choose values that describe one provider profile"
            ));
        }
        if let Some(legacy_api) = legacy_api
            && legacy_api != canonical.api_mode()
        {
            return Err(format!(
                "`{canonical_name}` conflicts with legacy `{legacy_api_name}`; remove the legacy field or choose values that describe one provider profile"
            ));
        }
    } else if legacy_metadata.is_some() || legacy_api.is_some() {
        model.provider_profile = Some(crate::config::ProviderProfile::from_legacy_modes(
            legacy_metadata.unwrap_or_else(|| inherited_profile.metadata_mode()),
            legacy_api.unwrap_or_else(|| inherited_profile.api_mode()),
        ));
    }
    model.provider_metadata_mode = None;
    model.provider_api_mode = None;
    Ok(())
}

pub(crate) fn normalize_request_timeout_alias(
    patch: &mut PartialResolvedConfig,
    canonical_name: &str,
    legacy_name: &str,
) -> Result<(), String> {
    let Some(model) = patch.model.as_mut() else {
        return Ok(());
    };
    let canonical = model.request_timeout_ms;
    let legacy = model.legacy_stream_idle_timeout_ms;
    if let (Some(canonical), Some(legacy)) = (canonical, legacy)
        && canonical != legacy
    {
        return Err(format!(
            "`{canonical_name}` and legacy `{legacy_name}` must match when both are set (got {canonical} and {legacy}); remove `{legacy_name}` or set both to one value"
        ));
    }
    model.request_timeout_ms = canonical.or(legacy);
    model.legacy_stream_idle_timeout_ms = None;
    Ok(())
}

fn apply_model(target: &mut crate::config::ModelConfig, patch: PartialModelConfig) {
    let next_profile = patch.provider_profile.or_else(|| {
        (patch.provider_metadata_mode.is_some() || patch.provider_api_mode.is_some()).then(|| {
            crate::config::ProviderProfile::from_legacy_modes(
                patch
                    .provider_metadata_mode
                    .unwrap_or_else(|| target.provider_profile.metadata_mode()),
                patch
                    .provider_api_mode
                    .unwrap_or_else(|| target.provider_profile.api_mode()),
            )
        })
    });
    let connection_target_changed = patch
        .base_url
        .as_deref()
        .is_some_and(|base_url| !same_provider_endpoint(&target.base_url, base_url))
        || next_profile.is_some_and(|profile| profile != target.provider_profile);
    if connection_target_changed {
        if patch.api_key_env.is_none() {
            target.api_key_env = None;
        }
        if patch.extra_headers.is_none() {
            target.extra_headers.clear();
        }
        if patch.extra_body_json.is_none() {
            target.extra_body_json = None;
        }
    }
    if let Some(value) = patch.base_url {
        target.base_url = value;
    }
    if let Some(value) = patch.model {
        target.model = value;
    }
    if let Some(value) = patch.provider_profile {
        target.provider_profile = value;
    }
    if patch.provider_metadata_mode.is_some() || patch.provider_api_mode.is_some() {
        target.provider_profile = crate::config::ProviderProfile::from_legacy_modes(
            patch
                .provider_metadata_mode
                .unwrap_or_else(|| target.provider_profile.metadata_mode()),
            patch
                .provider_api_mode
                .unwrap_or_else(|| target.provider_profile.api_mode()),
        );
    }
    if let Some(value) = patch.api_key_env {
        target.api_key_env = value;
    }
    if let Some(value) = patch.extra_headers {
        target.extra_headers = value;
    }
    if let Some(value) = patch.request_timeout_ms {
        target.request_timeout_ms = value;
    }
    if let Some(value) = patch.connect_timeout_ms {
        target.connect_timeout_ms = value;
    }
    if let Some(value) = patch.max_retries {
        target.max_retries = value;
    }
    if let Some(value) = patch.context_window {
        target.context_window = value;
    }
    if let Some(value) = patch.supports_tools {
        target.supports_tools = value;
    }
    if let Some(value) = patch.supports_images {
        target.supports_images = value;
    }
    if let Some(value) = patch.parallel_tool_calls {
        target.parallel_tool_calls = value;
    }
    if let Some(value) = patch.max_parallel_predictions {
        target.max_parallel_predictions = value.max(1);
    }
}

fn same_provider_endpoint(left: &str, right: &str) -> bool {
    match (
        ProviderEndpoint::parse(left),
        ProviderEndpoint::parse(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => left.trim() == right.trim(),
    }
}

fn apply_session(target: &mut crate::config::SessionConfig, patch: PartialSessionConfig) {
    if let Some(value) = patch.overflow_margin_tokens {
        target.overflow_margin_tokens = value;
    }
}

fn apply_multi_agent(target: &mut crate::config::MultiAgentConfig, patch: PartialMultiAgentConfig) {
    if let Some(value) = patch.enabled {
        target.enabled = value;
    }
    if let Some(value) = patch.mode {
        target.mode = value;
    }
    if let Some(value) = patch.max_concurrent_agents {
        target.max_concurrent_agents = value.max(1);
    }
    if let Some(value) = patch.max_concurrent_model_requests {
        target.max_concurrent_model_requests = value.max(1);
    }
}

fn apply_permissions(
    target: &mut crate::config::PermissionsConfig,
    patch: PartialPermissionsConfig,
) {
    if let Some(value) = patch.access_mode {
        target.access_mode = value;
    }
    if let Some(value) = patch.additional_read_roots {
        target.additional_read_roots = value;
    }
    if let Some(value) = patch.additional_write_roots {
        target.additional_write_roots = value;
    }
}

fn apply_shell(target: &mut crate::config::ShellConfig, patch: PartialShellConfig) {
    if let Some(value) = patch.program {
        target.program = value;
    }
    if let Some(value) = patch.family {
        target.family = value;
    }
    if let Some(value) = patch.default_timeout_ms {
        target.default_timeout_ms = value;
    }
    if let Some(value) = patch.max_timeout_ms {
        target.max_timeout_ms = value;
    }
    if let Some(value) = patch.env_allowlist {
        target.env_allowlist = value;
    }
    if let Some(value) = patch.hide_windows {
        target.hide_windows = value;
    }
}

fn apply_format(target: &mut crate::config::FormatConfig, patch: PartialFormatConfig) {
    if let Some(value) = patch.default_newline {
        target.default_newline = value;
    }
    if let Some(value) = patch.ensure_trailing_newline {
        target.ensure_trailing_newline = value;
    }
    if let Some(value) = patch.commands {
        target.commands = value;
    }
}

fn apply_instructions(
    target: &mut crate::config::InstructionConfig,
    patch: PartialInstructionConfig,
) {
    if let Some(value) = patch.additional_files {
        target.additional_files = value;
    }
}

fn apply_workspace(target: &mut crate::config::WorkspaceConfig, patch: PartialWorkspaceConfig) {
    if let Some(value) = patch.extra_ignore_globs {
        target.extra_ignore_globs = value;
    }
    if let Some(value) = patch.protected_paths {
        target.protected_paths = value;
    }
}

fn apply_inspection(
    target: &mut crate::config::model::InspectionConfig,
    patch: PartialInspectionConfig,
) {
    if let Some(value) = patch.default_max_depth {
        target.default_max_depth = value;
    }
    if let Some(value) = patch.default_max_entries_per_dir {
        target.default_max_entries_per_dir = value;
    }
    if let Some(value) = patch.max_extensions_reported {
        target.max_extensions_reported = value;
    }
    if let Some(value) = patch.include_hidden_by_default {
        target.include_hidden_by_default = value;
    }
}

fn apply_file_guard(
    target: &mut crate::config::model::FileGuardConfig,
    patch: PartialFileGuardConfig,
) {
    if let Some(value) = patch.max_inline_read_bytes {
        target.max_inline_read_bytes = value;
    }
    if let Some(value) = patch.large_file_warning_bytes {
        target.large_file_warning_bytes = value;
    }
    if let Some(value) = patch.blocked_read_extensions {
        target.blocked_read_extensions = value;
    }
    if let Some(value) = patch.structured_document_extensions {
        target.structured_document_extensions = value;
    }
}

fn apply_docling(target: &mut crate::config::model::DoclingConfig, patch: PartialDoclingConfig) {
    if let Some(value) = patch.enabled {
        target.enabled = value;
    }
    if let Some(value) = patch.base_url {
        target.base_url = value;
    }
    if let Some(value) = patch.timeout_ms {
        target.timeout_ms = value;
    }
    if let Some(value) = patch.api_key_env {
        target.api_key_env = value;
    }
    if let Some(value) = patch.headers {
        target.headers = value;
    }
}

fn apply_mcp(target: &mut crate::config::model::McpConfig, patch: PartialMcpConfig) {
    if let Some(value) = patch.enabled {
        target.enabled = value;
    }
    if let Some(value) = patch.servers {
        target.servers = value;
    }
}

fn apply_tool_output(target: &mut crate::config::ToolOutputConfig, patch: PartialToolOutputConfig) {
    if let Some(value) = patch.max_lines {
        target.max_lines = value;
    }
    if let Some(value) = patch.max_bytes {
        target.max_bytes = value;
    }
    if let Some(value) = patch.max_results {
        target.max_results = value;
    }
}

fn apply_logging(target: &mut crate::config::LoggingConfig, patch: PartialLoggingConfig) {
    if let Some(value) = patch.verbosity {
        target.verbosity = value;
    }
    if let Some(value) = patch.json_logs {
        target.json_logs = value;
    }
}

pub fn apply_patch(mut target: ResolvedConfig, patch: PartialResolvedConfig) -> ResolvedConfig {
    if let Some(value) = patch.model {
        apply_model(&mut target.model, value);
    }
    if let Some(value) = patch.session {
        apply_session(&mut target.session, value);
    }
    if let Some(value) = patch.multi_agent {
        apply_multi_agent(&mut target.multi_agent, value);
    }
    if let Some(value) = patch.permissions {
        apply_permissions(&mut target.permissions, value);
    }
    if let Some(value) = patch.shell {
        apply_shell(&mut target.shell, value);
    }
    if let Some(value) = patch.format {
        apply_format(&mut target.format, value);
    }
    if let Some(value) = patch.instructions {
        apply_instructions(&mut target.instructions, value);
    }
    if let Some(value) = patch.workspace {
        apply_workspace(&mut target.workspace, value);
    }
    if let Some(value) = patch.inspection {
        apply_inspection(&mut target.inspection, value);
    }
    if let Some(value) = patch.file_guard {
        apply_file_guard(&mut target.file_guard, value);
    }
    if let Some(value) = patch.docling {
        apply_docling(&mut target.docling, value);
    }
    if let Some(value) = patch.mcp {
        apply_mcp(&mut target.mcp, value);
    }
    if let Some(value) = patch.tool_output {
        apply_tool_output(&mut target.tool_output, value);
    }
    if let Some(value) = patch.logging {
        apply_logging(&mut target.logging, value);
    }
    target.model.clear_legacy_generation_settings();
    target
}

#[cfg(test)]
mod tests {
    use super::{apply_patch, normalize_provider_profile_alias, normalize_request_timeout_alias};
    use crate::config::model::{
        ChatCompletionsReasoningParameters, PartialModelConfig, PartialResolvedConfig,
        ProviderApiMode, ProviderMetadataMode, ProviderProfile, ReasoningEffort, ReasoningSummary,
        ResolvedConfig,
    };

    #[test]
    fn legacy_generation_patch_is_accepted_but_runtime_inert() {
        let defaults = ResolvedConfig::default();
        let expected_generation = defaults.model.clone();

        let resolved = apply_patch(
            defaults,
            PartialResolvedConfig {
                model: Some(PartialModelConfig {
                    provider_api_mode: Some(ProviderApiMode::ChatCompletions),
                    chat_completions_reasoning_parameters: Some(
                        ChatCompletionsReasoningParameters::EffortAndSummary,
                    ),
                    reasoning_effort: Some(ReasoningEffort::High),
                    reasoning_summary: Some(ReasoningSummary::Detailed),
                    max_output_tokens: Some(1),
                    temperature: Some(0.7),
                    top_p: Some(0.8),
                    top_k: Some(40),
                    presence_penalty: Some(0.1),
                    frequency_penalty: Some(0.2),
                    seed: Some(42),
                    stop_sequences: Some(vec!["legacy-stop".to_string()]),
                    supports_reasoning: Some(true),
                    extra_body_json: Some(serde_json::json!({
                        "chat_template_kwargs": { "enable_thinking": false }
                    })),
                    ..PartialModelConfig::default()
                }),
                ..PartialResolvedConfig::default()
            },
        );

        assert_eq!(
            resolved.model.provider_profile,
            ProviderProfile::LmStudioChatCompletions
        );
        assert_eq!(
            resolved.model.chat_completions_reasoning_parameters,
            expected_generation.chat_completions_reasoning_parameters
        );
        assert_eq!(
            resolved.model.reasoning_effort,
            expected_generation.reasoning_effort
        );
        assert_eq!(
            resolved.model.reasoning_summary,
            expected_generation.reasoning_summary
        );
        assert_eq!(
            resolved.model.max_output_tokens,
            expected_generation.max_output_tokens
        );
        assert_eq!(resolved.model.temperature, expected_generation.temperature);
        assert_eq!(resolved.model.top_p, expected_generation.top_p);
        assert_eq!(resolved.model.top_k, expected_generation.top_k);
        assert_eq!(
            resolved.model.presence_penalty,
            expected_generation.presence_penalty
        );
        assert_eq!(
            resolved.model.frequency_penalty,
            expected_generation.frequency_penalty
        );
        assert_eq!(resolved.model.seed, expected_generation.seed);
        assert_eq!(
            resolved.model.stop_sequences,
            expected_generation.stop_sequences
        );
        assert_eq!(
            resolved.model.supports_reasoning,
            expected_generation.supports_reasoning
        );
        assert_eq!(
            resolved.model.extra_body_json,
            expected_generation.extra_body_json
        );
    }

    #[test]
    fn provider_request_timeout_has_one_default_and_remains_overridable() {
        let defaults = ResolvedConfig::default();
        assert_eq!(defaults.model.request_timeout_ms, 3_600_000);
        assert_eq!(defaults.model.max_output_tokens, 32_768);

        let resolved = apply_patch(
            defaults,
            PartialResolvedConfig {
                model: Some(PartialModelConfig {
                    request_timeout_ms: Some(45_000),
                    ..PartialModelConfig::default()
                }),
                ..PartialResolvedConfig::default()
            },
        );

        assert_eq!(resolved.model.request_timeout_ms, 45_000);
    }

    #[test]
    fn provider_profile_alias_normalization_is_atomic_and_layer_aware() {
        let mut patch = PartialResolvedConfig {
            model: Some(PartialModelConfig {
                provider_metadata_mode: Some(ProviderMetadataMode::OpenAiCompatibleOnly),
                ..PartialModelConfig::default()
            }),
            ..PartialResolvedConfig::default()
        };

        normalize_provider_profile_alias(
            &mut patch,
            ProviderProfile::LmStudioChatCompletions,
            "model.provider_profile",
            "model.provider_metadata_mode",
            "model.provider_api_mode",
        )
        .expect("sparse legacy profile");

        let model = patch.model.expect("model patch");
        assert_eq!(
            model.provider_profile,
            Some(ProviderProfile::OpenAiCompatible)
        );
        assert_eq!(model.provider_metadata_mode, None);
        assert_eq!(model.provider_api_mode, None);
    }

    #[test]
    fn provider_target_layer_clears_only_credentials_it_does_not_explicitly_own() {
        let mut base = ResolvedConfig::default();
        base.model.base_url = "https://provider-a.example/v1".to_string();
        base.model.provider_profile = ProviderProfile::LmStudio;
        base.model.api_key_env = Some("PROVIDER_A_KEY".to_string());
        base.model.extra_headers = std::collections::BTreeMap::from([(
            "X-Provider-Secret".to_string(),
            "provider-a-secret".to_string(),
        )]);
        base.model.extra_body_json = Some(serde_json::json!({
            "api_key": "provider-a-body-secret"
        }));

        let changed_url = apply_patch(
            base.clone(),
            PartialResolvedConfig {
                model: Some(PartialModelConfig {
                    base_url: Some("https://provider-b.example/v1".to_string()),
                    ..PartialModelConfig::default()
                }),
                ..PartialResolvedConfig::default()
            },
        );
        assert_eq!(changed_url.model.api_key_env, None);
        assert!(changed_url.model.extra_headers.is_empty());
        assert_eq!(changed_url.model.extra_body_json, None);

        let same_canonical_url = apply_patch(
            base.clone(),
            PartialResolvedConfig {
                model: Some(PartialModelConfig {
                    base_url: Some("https://provider-a.example/v1/".to_string()),
                    ..PartialModelConfig::default()
                }),
                ..PartialResolvedConfig::default()
            },
        );
        assert_eq!(
            same_canonical_url.model.api_key_env.as_deref(),
            Some("PROVIDER_A_KEY")
        );
        assert_eq!(same_canonical_url.model.extra_headers.len(), 1);
        assert_eq!(same_canonical_url.model.extra_body_json, None);

        let changed_profile_with_explicit_key = apply_patch(
            base,
            PartialResolvedConfig {
                model: Some(PartialModelConfig {
                    provider_profile: Some(ProviderProfile::OpenAiCompatible),
                    api_key_env: Some(Some("PROVIDER_B_KEY".to_string())),
                    ..PartialModelConfig::default()
                }),
                ..PartialResolvedConfig::default()
            },
        );
        assert_eq!(
            changed_profile_with_explicit_key
                .model
                .api_key_env
                .as_deref(),
            Some("PROVIDER_B_KEY")
        );
        assert!(
            changed_profile_with_explicit_key
                .model
                .extra_headers
                .is_empty()
        );
        assert_eq!(
            changed_profile_with_explicit_key.model.extra_body_json,
            None
        );
    }

    #[test]
    fn legacy_stream_timeout_promotes_only_when_unambiguous() {
        for (canonical, legacy, expected) in [
            (None, Some(45_000), Some(45_000)),
            (Some(45_000), Some(45_000), Some(45_000)),
            (Some(45_000), None, Some(45_000)),
        ] {
            let mut patch = PartialResolvedConfig {
                model: Some(PartialModelConfig {
                    request_timeout_ms: canonical,
                    legacy_stream_idle_timeout_ms: legacy,
                    ..PartialModelConfig::default()
                }),
                ..PartialResolvedConfig::default()
            };

            normalize_request_timeout_alias(
                &mut patch,
                "model.request_timeout_ms",
                "model.stream_idle_timeout_ms",
            )
            .expect("compatible timeout fields");
            let model = patch.model.expect("model patch");
            assert_eq!(model.request_timeout_ms, expected);
            assert_eq!(model.legacy_stream_idle_timeout_ms, None);
        }
    }

    #[test]
    fn mismatched_legacy_stream_timeout_is_rejected_explicitly() {
        let mut patch = PartialResolvedConfig {
            model: Some(PartialModelConfig {
                request_timeout_ms: Some(45_000),
                legacy_stream_idle_timeout_ms: Some(20_000),
                ..PartialModelConfig::default()
            }),
            ..PartialResolvedConfig::default()
        };

        let error = normalize_request_timeout_alias(
            &mut patch,
            "model.request_timeout_ms",
            "model.stream_idle_timeout_ms",
        )
        .expect_err("different canonical and legacy values must fail");

        assert!(error.contains("model.request_timeout_ms"));
        assert!(error.contains("model.stream_idle_timeout_ms"));
        assert!(error.contains("45000"));
        assert!(error.contains("20000"));
    }
}
