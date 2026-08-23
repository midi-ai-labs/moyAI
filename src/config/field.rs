#[cfg(any(feature = "tauri-desktop", test))]
use std::collections::HashSet;

use super::merge::apply_patch as apply_config_patch;
use super::model::{
    AccessMode, MAX_MODEL_REQUEST_TIMEOUT_MS, McpServerConfig, MultiAgentMode,
    PartialDoclingConfig, PartialFileGuardConfig, PartialInspectionConfig, PartialMcpConfig,
    PartialModelConfig, PartialMultiAgentConfig, PartialPermissionsConfig, PartialResolvedConfig,
    PartialShellConfig, ProviderProfile, ResolvedConfig, validate_optional_provider_float,
};
use super::turn::ProviderEndpoint;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigField {
    BaseUrl,
    Model,
    ProviderProfile,
    ApiKeyEnv,
    AccessMode,
    MultiAgentEnabled,
    MultiAgentMode,
    MultiAgentMaxAgents,
    MultiAgentMaxModelRequests,
    Temperature,
    TopP,
    TopK,
    PresencePenalty,
    FrequencyPenalty,
    Seed,
    StopSequences,
    ContextWindow,
    MaxOutputTokens,
    RequestTimeoutMs,
    ConnectTimeoutMs,
    MaxRetries,
    SupportsTools,
    SupportsReasoning,
    SupportsImages,
    ParallelToolCalls,
    MaxParallelPredictions,
    ExtraHeadersJson,
    ExtraBodyJson,
    ShellHideWindows,
    InspectionDefaultMaxDepth,
    InspectionDefaultMaxEntriesPerDir,
    InspectionMaxExtensionsReported,
    InspectionIncludeHiddenByDefault,
    FileGuardMaxInlineReadBytes,
    FileGuardLargeFileWarningBytes,
    FileGuardBlockedReadExtensions,
    FileGuardStructuredDocumentExtensions,
    DoclingEnabled,
    DoclingBaseUrl,
    DoclingTimeoutMs,
    DoclingApiKeyEnv,
    DoclingHeadersJson,
    McpEnabled,
    McpServersJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFieldValueType {
    String,
    Boolean,
    Integer,
    Number,
    Json,
    Enum,
}

impl ConfigFieldValueType {
    #[cfg(any(feature = "tauri-desktop", test))]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Json => "json",
            Self::Enum => "enum",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigFieldDescriptor {
    field: ConfigField,
    value_type: ConfigFieldValueType,
    integer_min: Option<u64>,
    integer_max: Option<u64>,
    options: &'static [&'static str],
}

impl ConfigFieldDescriptor {
    pub(crate) fn key(self) -> &'static str {
        self.field.label()
    }

    #[cfg(any(feature = "tauri-desktop", test))]
    pub(crate) fn env_override(self) -> Option<&'static str> {
        self.field.env_override()
    }

    #[cfg(any(feature = "tauri-desktop", test))]
    pub(crate) const fn value_type(self) -> ConfigFieldValueType {
        self.value_type
    }

    pub(crate) fn required(self) -> bool {
        !self.field.allows_empty_complete_value()
    }

    pub(crate) const fn integer_min(self) -> Option<u64> {
        self.integer_min
    }

    pub(crate) const fn integer_max(self) -> Option<u64> {
        self.integer_max
    }

    #[cfg(any(feature = "tauri-desktop", test))]
    pub(crate) const fn options(self) -> &'static [&'static str] {
        self.options
    }
}

impl ConfigField {
    pub const ALL: [ConfigField; 44] = [
        ConfigField::BaseUrl,
        ConfigField::Model,
        ConfigField::ProviderProfile,
        ConfigField::ApiKeyEnv,
        ConfigField::AccessMode,
        ConfigField::MultiAgentEnabled,
        ConfigField::MultiAgentMode,
        ConfigField::MultiAgentMaxAgents,
        ConfigField::MultiAgentMaxModelRequests,
        ConfigField::Temperature,
        ConfigField::TopP,
        ConfigField::TopK,
        ConfigField::PresencePenalty,
        ConfigField::FrequencyPenalty,
        ConfigField::Seed,
        ConfigField::StopSequences,
        ConfigField::ContextWindow,
        ConfigField::MaxOutputTokens,
        ConfigField::RequestTimeoutMs,
        ConfigField::ConnectTimeoutMs,
        ConfigField::MaxRetries,
        ConfigField::SupportsTools,
        ConfigField::SupportsReasoning,
        ConfigField::SupportsImages,
        ConfigField::ParallelToolCalls,
        ConfigField::MaxParallelPredictions,
        ConfigField::ExtraHeadersJson,
        ConfigField::ExtraBodyJson,
        ConfigField::ShellHideWindows,
        ConfigField::InspectionDefaultMaxDepth,
        ConfigField::InspectionDefaultMaxEntriesPerDir,
        ConfigField::InspectionMaxExtensionsReported,
        ConfigField::InspectionIncludeHiddenByDefault,
        ConfigField::FileGuardMaxInlineReadBytes,
        ConfigField::FileGuardLargeFileWarningBytes,
        ConfigField::FileGuardBlockedReadExtensions,
        ConfigField::FileGuardStructuredDocumentExtensions,
        ConfigField::DoclingEnabled,
        ConfigField::DoclingBaseUrl,
        ConfigField::DoclingTimeoutMs,
        ConfigField::DoclingApiKeyEnv,
        ConfigField::DoclingHeadersJson,
        ConfigField::McpEnabled,
        ConfigField::McpServersJson,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConfigField::BaseUrl => "model.base_url",
            ConfigField::Model => "model.model",
            ConfigField::ProviderProfile => "model.provider_profile",
            ConfigField::ApiKeyEnv => "model.api_key_env",
            ConfigField::AccessMode => "permissions.access_mode",
            ConfigField::MultiAgentEnabled => "multi_agent.enabled",
            ConfigField::MultiAgentMode => "multi_agent.mode",
            ConfigField::MultiAgentMaxAgents => "multi_agent.max_concurrent_agents",
            ConfigField::MultiAgentMaxModelRequests => "multi_agent.max_concurrent_model_requests",
            ConfigField::Temperature => "model.temperature",
            ConfigField::TopP => "model.top_p",
            ConfigField::TopK => "model.top_k",
            ConfigField::PresencePenalty => "model.presence_penalty",
            ConfigField::FrequencyPenalty => "model.frequency_penalty",
            ConfigField::Seed => "model.seed",
            ConfigField::StopSequences => "model.stop_sequences",
            ConfigField::ContextWindow => "model.context_window",
            ConfigField::MaxOutputTokens => "model.max_output_tokens",
            ConfigField::RequestTimeoutMs => "model.request_timeout_ms",
            ConfigField::ConnectTimeoutMs => "model.connect_timeout_ms",
            ConfigField::MaxRetries => "model.max_retries",
            ConfigField::SupportsTools => "model.supports_tools",
            ConfigField::SupportsReasoning => "model.supports_reasoning",
            ConfigField::SupportsImages => "model.supports_images",
            ConfigField::ParallelToolCalls => "model.parallel_tool_calls",
            ConfigField::MaxParallelPredictions => "model.max_parallel_predictions",
            ConfigField::ExtraHeadersJson => "model.extra_headers_json",
            ConfigField::ExtraBodyJson => "model.extra_body_json",
            ConfigField::ShellHideWindows => "shell.hide_windows",
            ConfigField::InspectionDefaultMaxDepth => "inspection.default_max_depth",
            ConfigField::InspectionDefaultMaxEntriesPerDir => {
                "inspection.default_max_entries_per_dir"
            }
            ConfigField::InspectionMaxExtensionsReported => "inspection.max_extensions_reported",
            ConfigField::InspectionIncludeHiddenByDefault => "inspection.include_hidden_by_default",
            ConfigField::FileGuardMaxInlineReadBytes => "file_guard.max_inline_read_bytes",
            ConfigField::FileGuardLargeFileWarningBytes => "file_guard.large_file_warning_bytes",
            ConfigField::FileGuardBlockedReadExtensions => "file_guard.blocked_read_extensions",
            ConfigField::FileGuardStructuredDocumentExtensions => {
                "file_guard.structured_document_extensions"
            }
            ConfigField::DoclingEnabled => "docling.enabled",
            ConfigField::DoclingBaseUrl => "docling.base_url",
            ConfigField::DoclingTimeoutMs => "docling.timeout_ms",
            ConfigField::DoclingApiKeyEnv => "docling.api_key_env",
            ConfigField::DoclingHeadersJson => "docling.headers_json",
            ConfigField::McpEnabled => "mcp.enabled",
            ConfigField::McpServersJson => "mcp.servers_json",
        }
    }

    pub fn env_override(self) -> Option<&'static str> {
        match self {
            ConfigField::BaseUrl => Some("MOYAI_BASE_URL"),
            ConfigField::Model => Some("MOYAI_MODEL"),
            ConfigField::ProviderProfile => Some("MOYAI_PROVIDER_PROFILE"),
            ConfigField::ApiKeyEnv => Some("MOYAI_API_KEY_ENV"),
            ConfigField::AccessMode => Some("MOYAI_ACCESS_MODE"),
            ConfigField::MultiAgentEnabled => Some("MOYAI_MULTI_AGENT_ENABLED"),
            ConfigField::MultiAgentMode => Some("MOYAI_MULTI_AGENT_MODE"),
            ConfigField::MultiAgentMaxAgents => Some("MOYAI_MULTI_AGENT_MAX_AGENTS"),
            ConfigField::MultiAgentMaxModelRequests => Some("MOYAI_MULTI_AGENT_MAX_MODEL_REQUESTS"),
            ConfigField::Temperature => Some("MOYAI_TEMPERATURE"),
            ConfigField::TopP => Some("MOYAI_TOP_P"),
            ConfigField::TopK => Some("MOYAI_TOP_K"),
            ConfigField::PresencePenalty => Some("MOYAI_PRESENCE_PENALTY"),
            ConfigField::FrequencyPenalty => Some("MOYAI_FREQUENCY_PENALTY"),
            ConfigField::Seed => Some("MOYAI_SEED"),
            ConfigField::StopSequences => Some("MOYAI_STOP_SEQUENCES"),
            ConfigField::ContextWindow => Some("MOYAI_CONTEXT_WINDOW"),
            ConfigField::MaxOutputTokens => Some("MOYAI_MAX_OUTPUT_TOKENS"),
            ConfigField::RequestTimeoutMs => Some("MOYAI_REQUEST_TIMEOUT_MS"),
            ConfigField::ConnectTimeoutMs => Some("MOYAI_CONNECT_TIMEOUT_MS"),
            ConfigField::MaxRetries => Some("MOYAI_MAX_RETRIES"),
            ConfigField::SupportsTools => Some("MOYAI_SUPPORTS_TOOLS"),
            ConfigField::SupportsReasoning => Some("MOYAI_SUPPORTS_REASONING"),
            ConfigField::SupportsImages => Some("MOYAI_SUPPORTS_IMAGES"),
            ConfigField::ParallelToolCalls => Some("MOYAI_PARALLEL_TOOL_CALLS"),
            ConfigField::MaxParallelPredictions => Some("MOYAI_MAX_PARALLEL_PREDICTIONS"),
            ConfigField::ExtraHeadersJson => Some("MOYAI_EXTRA_HEADERS"),
            ConfigField::ExtraBodyJson => Some("MOYAI_EXTRA_BODY_JSON"),
            ConfigField::ShellHideWindows => Some("MOYAI_SHELL_HIDE_WINDOWS"),
            ConfigField::InspectionDefaultMaxDepth => Some("MOYAI_INSPECTION_MAX_DEPTH"),
            ConfigField::InspectionDefaultMaxEntriesPerDir => {
                Some("MOYAI_INSPECTION_MAX_ENTRIES_PER_DIR")
            }
            ConfigField::InspectionMaxExtensionsReported => {
                Some("MOYAI_INSPECTION_MAX_EXTENSIONS_REPORTED")
            }
            ConfigField::InspectionIncludeHiddenByDefault => {
                Some("MOYAI_INSPECTION_INCLUDE_HIDDEN")
            }
            ConfigField::FileGuardMaxInlineReadBytes => Some("MOYAI_MAX_INLINE_READ_BYTES"),
            ConfigField::FileGuardLargeFileWarningBytes => Some("MOYAI_LARGE_FILE_WARNING_BYTES"),
            ConfigField::FileGuardBlockedReadExtensions => Some("MOYAI_BLOCKED_READ_EXTENSIONS"),
            ConfigField::FileGuardStructuredDocumentExtensions => {
                Some("MOYAI_STRUCTURED_DOCUMENT_EXTENSIONS")
            }
            ConfigField::DoclingEnabled => Some("MOYAI_DOCLING_ENABLED"),
            ConfigField::DoclingBaseUrl => Some("MOYAI_DOCLING_BASE_URL"),
            ConfigField::DoclingTimeoutMs => Some("MOYAI_DOCLING_TIMEOUT_MS"),
            ConfigField::DoclingApiKeyEnv => Some("MOYAI_DOCLING_API_KEY_ENV"),
            ConfigField::DoclingHeadersJson => Some("MOYAI_DOCLING_HEADERS"),
            ConfigField::McpEnabled => Some("MOYAI_MCP_ENABLED"),
            ConfigField::McpServersJson => Some("MOYAI_MCP_SERVERS_JSON"),
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            ConfigField::ProviderProfile => "Connection type",
            ConfigField::ApiKeyEnv => "API key environment variable (optional)",
            ConfigField::RequestTimeoutMs => "LLM response timeout",
            _ => self.label(),
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            ConfigField::ProviderProfile => {
                "モデル一覧の取得方式と生成APIを一つの接続方式として選びます。oMLX等の一般的なOpenAI互換serverにはOpenAI-compatible (Chat Completions)を選択します。"
            }
            ConfigField::ApiKeyEnv => {
                "API keyそのものではなく、起動環境に設定した環境変数名（例: OPENAI_API_KEY）を入力します。認証不要なら空欄にします。"
            }
            ConfigField::RequestTimeoutMs => {
                "最初の送信開始からstream完了までのLLM応答全体に適用する総上限（ms）です。"
            }
            _ => "",
        }
    }

    pub(crate) fn toml_path(self) -> (&'static str, &'static str) {
        match self {
            ConfigField::ExtraHeadersJson => ("model", "extra_headers"),
            ConfigField::DoclingHeadersJson => ("docling", "headers"),
            ConfigField::McpServersJson => ("mcp", "servers"),
            _ => self
                .label()
                .split_once('.')
                .expect("config field labels are section-qualified"),
        }
    }

    pub(crate) fn descriptor(self) -> ConfigFieldDescriptor {
        const NONE: &[&str] = &[];
        const PROVIDER_PROFILES: &[&str] = &[
            "lm_studio",
            "openai_compatible",
            "openai_responses",
            "lm_studio_chat_completions",
        ];
        const ACCESS_MODES: &[&str] = &["default", "auto_review", "full_access"];
        const MULTI_AGENT_MODES: &[&str] = &["explicit_request_only", "proactive"];

        let (value_type, integer_min, integer_max, options) = match self {
            ConfigField::ProviderProfile => {
                (ConfigFieldValueType::Enum, None, None, PROVIDER_PROFILES)
            }
            ConfigField::AccessMode => (ConfigFieldValueType::Enum, None, None, ACCESS_MODES),
            ConfigField::MultiAgentMode => {
                (ConfigFieldValueType::Enum, None, None, MULTI_AGENT_MODES)
            }
            ConfigField::MultiAgentEnabled
            | ConfigField::SupportsTools
            | ConfigField::SupportsReasoning
            | ConfigField::SupportsImages
            | ConfigField::ParallelToolCalls
            | ConfigField::ShellHideWindows
            | ConfigField::InspectionIncludeHiddenByDefault
            | ConfigField::DoclingEnabled
            | ConfigField::McpEnabled => (ConfigFieldValueType::Boolean, None, None, NONE),
            ConfigField::Temperature
            | ConfigField::TopP
            | ConfigField::PresencePenalty
            | ConfigField::FrequencyPenalty => (ConfigFieldValueType::Number, None, None, NONE),
            ConfigField::ExtraHeadersJson
            | ConfigField::ExtraBodyJson
            | ConfigField::DoclingHeadersJson
            | ConfigField::McpServersJson => (ConfigFieldValueType::Json, None, None, NONE),
            ConfigField::MultiAgentMaxAgents | ConfigField::MultiAgentMaxModelRequests => {
                (ConfigFieldValueType::Integer, Some(1), None, NONE)
            }
            ConfigField::ContextWindow | ConfigField::MaxParallelPredictions => (
                ConfigFieldValueType::Integer,
                Some(1),
                Some(u32::MAX as u64),
                NONE,
            ),
            ConfigField::TopK | ConfigField::MaxOutputTokens => (
                ConfigFieldValueType::Integer,
                Some(0),
                Some(u32::MAX as u64),
                NONE,
            ),
            ConfigField::MaxRetries => (
                ConfigFieldValueType::Integer,
                Some(0),
                Some(u8::MAX as u64),
                NONE,
            ),
            ConfigField::InspectionDefaultMaxDepth
            | ConfigField::InspectionDefaultMaxEntriesPerDir
            | ConfigField::InspectionMaxExtensionsReported => {
                (ConfigFieldValueType::Integer, Some(0), None, NONE)
            }
            ConfigField::RequestTimeoutMs => (
                ConfigFieldValueType::Integer,
                Some(1),
                Some(MAX_MODEL_REQUEST_TIMEOUT_MS),
                NONE,
            ),
            ConfigField::ConnectTimeoutMs
            | ConfigField::FileGuardMaxInlineReadBytes
            | ConfigField::FileGuardLargeFileWarningBytes
            | ConfigField::DoclingTimeoutMs
            | ConfigField::Seed => (ConfigFieldValueType::Integer, Some(0), None, NONE),
            ConfigField::BaseUrl
            | ConfigField::Model
            | ConfigField::ApiKeyEnv
            | ConfigField::StopSequences
            | ConfigField::FileGuardBlockedReadExtensions
            | ConfigField::FileGuardStructuredDocumentExtensions
            | ConfigField::DoclingBaseUrl
            | ConfigField::DoclingApiKeyEnv => (ConfigFieldValueType::String, None, None, NONE),
        };
        ConfigFieldDescriptor {
            field: self,
            value_type,
            integer_min,
            integer_max,
            options,
        }
    }

    pub(crate) fn allows_empty_complete_value(self) -> bool {
        matches!(
            self,
            ConfigField::Temperature
                | ConfigField::TopP
                | ConfigField::TopK
                | ConfigField::PresencePenalty
                | ConfigField::FrequencyPenalty
                | ConfigField::Seed
                | ConfigField::ApiKeyEnv
                | ConfigField::StopSequences
                | ConfigField::ExtraHeadersJson
                | ConfigField::ExtraBodyJson
                | ConfigField::FileGuardBlockedReadExtensions
                | ConfigField::FileGuardStructuredDocumentExtensions
                | ConfigField::DoclingApiKeyEnv
                | ConfigField::DoclingHeadersJson
                | ConfigField::McpServersJson
        )
    }

    pub(crate) fn value(self, config: &ResolvedConfig) -> String {
        match self {
            ConfigField::BaseUrl => config.model.base_url.clone(),
            ConfigField::Model => config.model.model.clone(),
            ConfigField::ProviderProfile => config.model.provider_profile.as_str().to_string(),
            ConfigField::ApiKeyEnv => config.model.api_key_env.clone().unwrap_or_default(),
            ConfigField::AccessMode => config.permissions.access_mode.as_str().to_string(),
            ConfigField::MultiAgentEnabled => config.multi_agent.enabled.to_string(),
            ConfigField::MultiAgentMode => config.multi_agent.mode.as_str().to_string(),
            ConfigField::MultiAgentMaxAgents => {
                config.multi_agent.max_concurrent_agents.to_string()
            }
            ConfigField::MultiAgentMaxModelRequests => {
                config.multi_agent.max_concurrent_model_requests.to_string()
            }
            ConfigField::Temperature => option_value(config.model.temperature),
            ConfigField::TopP => option_value(config.model.top_p),
            ConfigField::TopK => option_value(config.model.top_k),
            ConfigField::PresencePenalty => option_value(config.model.presence_penalty),
            ConfigField::FrequencyPenalty => option_value(config.model.frequency_penalty),
            ConfigField::Seed => option_value(config.model.seed),
            ConfigField::StopSequences => config.model.stop_sequences.join(", "),
            ConfigField::ContextWindow => config.model.context_window.to_string(),
            ConfigField::MaxOutputTokens => config.model.max_output_tokens.to_string(),
            ConfigField::RequestTimeoutMs => config.model.request_timeout_ms.to_string(),
            ConfigField::ConnectTimeoutMs => config.model.connect_timeout_ms.to_string(),
            ConfigField::MaxRetries => config.model.max_retries.to_string(),
            ConfigField::SupportsTools => config.model.supports_tools.to_string(),
            ConfigField::SupportsReasoning => config.model.supports_reasoning.to_string(),
            ConfigField::SupportsImages => config.model.supports_images.to_string(),
            ConfigField::ParallelToolCalls => config.model.parallel_tool_calls.to_string(),
            ConfigField::MaxParallelPredictions => {
                config.model.max_parallel_predictions.to_string()
            }
            ConfigField::ExtraHeadersJson => {
                serde_json::to_string(&config.model.extra_headers).unwrap_or_default()
            }
            ConfigField::ExtraBodyJson => config
                .model
                .extra_body_json
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            ConfigField::ShellHideWindows => config.shell.hide_windows.to_string(),
            ConfigField::InspectionDefaultMaxDepth => {
                config.inspection.default_max_depth.to_string()
            }
            ConfigField::InspectionDefaultMaxEntriesPerDir => {
                config.inspection.default_max_entries_per_dir.to_string()
            }
            ConfigField::InspectionMaxExtensionsReported => {
                config.inspection.max_extensions_reported.to_string()
            }
            ConfigField::InspectionIncludeHiddenByDefault => {
                config.inspection.include_hidden_by_default.to_string()
            }
            ConfigField::FileGuardMaxInlineReadBytes => {
                config.file_guard.max_inline_read_bytes.to_string()
            }
            ConfigField::FileGuardLargeFileWarningBytes => {
                config.file_guard.large_file_warning_bytes.to_string()
            }
            ConfigField::FileGuardBlockedReadExtensions => {
                config.file_guard.blocked_read_extensions.join(", ")
            }
            ConfigField::FileGuardStructuredDocumentExtensions => {
                config.file_guard.structured_document_extensions.join(", ")
            }
            ConfigField::DoclingEnabled => config.docling.enabled.to_string(),
            ConfigField::DoclingBaseUrl => config.docling.base_url.clone(),
            ConfigField::DoclingTimeoutMs => config.docling.timeout_ms.to_string(),
            ConfigField::DoclingApiKeyEnv => config.docling.api_key_env.clone().unwrap_or_default(),
            ConfigField::DoclingHeadersJson => {
                serde_json::to_string(&config.docling.headers).unwrap_or_default()
            }
            ConfigField::McpEnabled => config.mcp.enabled.to_string(),
            ConfigField::McpServersJson => {
                serde_json::to_string(&config.mcp.servers).unwrap_or_default()
            }
        }
    }
}

pub(crate) fn build_resolved_config_from_field_values(
    base: &ResolvedConfig,
    fields: &[(ConfigField, &str)],
) -> Result<ResolvedConfig, String> {
    validate_complete_config_field_values(fields)?;
    let mut config = apply_config_patch(base.clone(), parse_config_field_patch(fields)?);

    for (field, value) in fields {
        if !value.trim().is_empty() {
            continue;
        }
        match field {
            ConfigField::Temperature => config.model.temperature = None,
            ConfigField::TopP => config.model.top_p = None,
            ConfigField::TopK => config.model.top_k = None,
            ConfigField::PresencePenalty => config.model.presence_penalty = None,
            ConfigField::FrequencyPenalty => config.model.frequency_penalty = None,
            ConfigField::Seed => config.model.seed = None,
            ConfigField::ApiKeyEnv => config.model.api_key_env = None,
            ConfigField::ExtraHeadersJson => config.model.extra_headers.clear(),
            ConfigField::ExtraBodyJson => config.model.extra_body_json = None,
            ConfigField::DoclingApiKeyEnv => config.docling.api_key_env = None,
            ConfigField::DoclingHeadersJson => config.docling.headers.clear(),
            ConfigField::McpServersJson => config.mcp.servers.clear(),
            _ => {}
        }
    }

    config.normalize_and_validate_provider_runtime()?;
    config.normalize_and_validate_docling_runtime()?;
    Ok(config)
}

#[cfg(any(feature = "tauri-desktop", test))]
pub(crate) fn build_resolved_config_from_key_values(
    base: &ResolvedConfig,
    values: Vec<(String, String)>,
) -> Result<ResolvedConfig, String> {
    let mut fields = ConfigField::ALL
        .into_iter()
        .map(|field| (field, field.value(base)))
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    for (key, value) in values {
        if !seen.insert(key.clone()) {
            return Err(format!("duplicate config field key: {key}"));
        }
        let field = fields
            .iter_mut()
            .find(|(field, _)| field.label() == key)
            .ok_or_else(|| format!("unknown config field key: {key}"))?;
        field.1 = value;
    }
    let borrowed = fields
        .iter()
        .map(|(field, value)| (*field, value.as_str()))
        .collect::<Vec<_>>();
    build_resolved_config_from_field_values(base, &borrowed)
}

pub(crate) fn parse_config_field_patch(
    fields: &[(ConfigField, &str)],
) -> Result<PartialResolvedConfig, String> {
    let mut patch = PartialResolvedConfig::default();
    let mut model = PartialModelConfig::default();
    let mut permissions = PartialPermissionsConfig::default();
    let mut multi_agent = PartialMultiAgentConfig::default();
    let mut shell = PartialShellConfig::default();
    let mut inspection = PartialInspectionConfig::default();
    let mut file_guard = PartialFileGuardConfig::default();
    let mut docling = PartialDoclingConfig::default();
    let mut mcp = PartialMcpConfig::default();

    for (field, value) in fields {
        let text = value.trim();
        match field {
            ConfigField::BaseUrl => {
                model.base_url = match parse_string(text) {
                    Some(value) => Some(
                        ProviderEndpoint::parse(&value)
                            .map_err(|error| error.to_string())?
                            .as_str()
                            .to_string(),
                    ),
                    None => None,
                }
            }
            ConfigField::Model => model.model = parse_string(text),
            ConfigField::ProviderProfile => {
                model.provider_profile = match parse_string(text) {
                    Some(value) => Some(parse_provider_profile(&value)?),
                    None => None,
                }
            }
            ConfigField::ApiKeyEnv => model.api_key_env = Some(parse_string(text)),
            ConfigField::AccessMode => {
                permissions.access_mode = match parse_string(text) {
                    Some(value) => Some(parse_access_mode(&value)?),
                    None => None,
                }
            }
            ConfigField::MultiAgentEnabled => multi_agent.enabled = parse_bool(text)?,
            ConfigField::MultiAgentMode => {
                multi_agent.mode = match parse_string(text) {
                    Some(value) => Some(parse_multi_agent_mode(&value)?),
                    None => None,
                }
            }
            ConfigField::MultiAgentMaxAgents => {
                multi_agent.max_concurrent_agents = parse_integer(text, field.descriptor())?
            }
            ConfigField::MultiAgentMaxModelRequests => {
                multi_agent.max_concurrent_model_requests = parse_integer(text, field.descriptor())?
            }
            ConfigField::Temperature => model.temperature = parse_provider_float(text, *field)?,
            ConfigField::TopP => model.top_p = parse_provider_float(text, *field)?,
            ConfigField::TopK => model.top_k = parse_integer(text, field.descriptor())?,
            ConfigField::PresencePenalty => {
                model.presence_penalty = parse_provider_float(text, *field)?
            }
            ConfigField::FrequencyPenalty => {
                model.frequency_penalty = parse_provider_float(text, *field)?
            }
            ConfigField::Seed => model.seed = parse_integer(text, field.descriptor())?,
            ConfigField::StopSequences => model.stop_sequences = Some(parse_csv(text)),
            ConfigField::ContextWindow => {
                model.context_window = parse_integer(text, field.descriptor())?
            }
            ConfigField::MaxOutputTokens => {
                model.max_output_tokens = parse_integer(text, field.descriptor())?
            }
            ConfigField::RequestTimeoutMs => {
                model.request_timeout_ms = parse_integer(text, field.descriptor())?
            }
            ConfigField::ConnectTimeoutMs => {
                model.connect_timeout_ms = parse_integer(text, field.descriptor())?
            }
            ConfigField::MaxRetries => model.max_retries = parse_integer(text, field.descriptor())?,
            ConfigField::SupportsTools => model.supports_tools = parse_bool(text)?,
            ConfigField::SupportsReasoning => model.supports_reasoning = parse_bool(text)?,
            ConfigField::SupportsImages => model.supports_images = parse_bool(text)?,
            ConfigField::ParallelToolCalls => model.parallel_tool_calls = parse_bool(text)?,
            ConfigField::MaxParallelPredictions => {
                model.max_parallel_predictions = parse_integer(text, field.descriptor())?
            }
            ConfigField::ExtraHeadersJson => {
                model.extra_headers = match parse_string(text) {
                    Some(value) => Some(
                        serde_json::from_str(&value)
                            .map_err(|error| format!("extra_headers_json: {error}"))?,
                    ),
                    None => None,
                }
            }
            ConfigField::ExtraBodyJson => {
                model.extra_body_json = match parse_string(text) {
                    Some(value) => Some(
                        serde_json::from_str(&value)
                            .map_err(|error| format!("extra_body_json: {error}"))?,
                    ),
                    None => None,
                }
            }
            ConfigField::ShellHideWindows => shell.hide_windows = parse_bool(text)?,
            ConfigField::InspectionDefaultMaxDepth => {
                inspection.default_max_depth = parse_integer(text, field.descriptor())?
            }
            ConfigField::InspectionDefaultMaxEntriesPerDir => {
                inspection.default_max_entries_per_dir = parse_integer(text, field.descriptor())?
            }
            ConfigField::InspectionMaxExtensionsReported => {
                inspection.max_extensions_reported = parse_integer(text, field.descriptor())?
            }
            ConfigField::InspectionIncludeHiddenByDefault => {
                inspection.include_hidden_by_default = parse_bool(text)?
            }
            ConfigField::FileGuardMaxInlineReadBytes => {
                file_guard.max_inline_read_bytes = parse_integer(text, field.descriptor())?
            }
            ConfigField::FileGuardLargeFileWarningBytes => {
                file_guard.large_file_warning_bytes = parse_integer(text, field.descriptor())?
            }
            ConfigField::FileGuardBlockedReadExtensions => {
                file_guard.blocked_read_extensions = Some(parse_extension_csv(text))
            }
            ConfigField::FileGuardStructuredDocumentExtensions => {
                file_guard.structured_document_extensions = Some(parse_extension_csv(text))
            }
            ConfigField::DoclingEnabled => docling.enabled = parse_bool(text)?,
            ConfigField::DoclingBaseUrl => docling.base_url = parse_string(text),
            ConfigField::DoclingTimeoutMs => {
                docling.timeout_ms = parse_integer(text, field.descriptor())?
            }
            ConfigField::DoclingApiKeyEnv => docling.api_key_env = Some(parse_string(text)),
            ConfigField::DoclingHeadersJson => {
                docling.headers = match parse_string(text) {
                    Some(value) => Some(
                        serde_json::from_str(&value)
                            .map_err(|error| format!("docling.headers_json: {error}"))?,
                    ),
                    None => None,
                }
            }
            ConfigField::McpEnabled => mcp.enabled = parse_bool(text)?,
            ConfigField::McpServersJson => {
                mcp.servers = match parse_string(text) {
                    Some(value) => Some(
                        serde_json::from_str::<Vec<McpServerConfig>>(&value)
                            .map_err(|error| format!("mcp.servers_json: {error}"))?,
                    ),
                    None => None,
                }
            }
        }
    }

    patch.model = Some(model);
    patch.permissions = Some(permissions);
    patch.multi_agent = Some(multi_agent);
    patch.shell = Some(shell);
    patch.inspection = Some(inspection);
    patch.file_guard = Some(file_guard);
    patch.docling = Some(docling);
    patch.mcp = Some(mcp);
    Ok(patch)
}

fn validate_complete_config_field_values(fields: &[(ConfigField, &str)]) -> Result<(), String> {
    for (field, value) in fields {
        if !value.trim().is_empty() || !field.descriptor().required() {
            continue;
        }
        return Err(format!("{} must not be empty", field.label()));
    }
    Ok(())
}

fn parse_provider_profile(value: &str) -> Result<ProviderProfile, String> {
    ProviderProfile::parse(value).ok_or_else(|| {
        format!(
            "unsupported provider_profile `{}`",
            value.trim().to_ascii_lowercase()
        )
    })
}

fn parse_access_mode(value: &str) -> Result<AccessMode, String> {
    AccessMode::parse(value).ok_or_else(|| {
        format!(
            "unsupported access_mode `{}`",
            value.trim().to_ascii_lowercase()
        )
    })
}

fn parse_multi_agent_mode(value: &str) -> Result<MultiAgentMode, String> {
    MultiAgentMode::parse(&value.to_ascii_lowercase())
        .ok_or_else(|| format!("unsupported multi_agent.mode `{value}`"))
}

fn parse_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_extension_csv(value: &str) -> Vec<String> {
    parse_csv(value)
        .into_iter()
        .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
        .collect()
}

fn parse_bool(value: &str) -> Result<Option<bool>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<bool>()
        .map(Some)
        .map_err(|error| error.to_string())
}

fn parse_number<T>(value: &str) -> Result<Option<T>, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<T>()
        .map(Some)
        .map_err(|error| error.to_string())
}

fn parse_integer<T>(value: &str, descriptor: ConfigFieldDescriptor) -> Result<Option<T>, String>
where
    T: TryFrom<u64>,
    T::Error: std::fmt::Display,
{
    let parsed = parse_number::<u64>(value)?;
    let Some(parsed) = parsed else {
        return Ok(None);
    };
    match (descriptor.integer_min(), descriptor.integer_max()) {
        (Some(minimum), Some(maximum)) if !(minimum..=maximum).contains(&parsed) => {
            return Err(format!(
                "{} must be between {minimum} and {maximum} inclusive",
                descriptor.key(),
            ));
        }
        (Some(minimum), None) if parsed < minimum => {
            return Err(format!("{} must be at least {minimum}", descriptor.key(),));
        }
        (None, Some(maximum)) if parsed > maximum => {
            return Err(format!("{} must be at most {maximum}", descriptor.key(),));
        }
        _ => {}
    }
    T::try_from(parsed).map(Some).map_err(|error| {
        format!(
            "{} is outside the supported integer range: {error}",
            descriptor.key(),
        )
    })
}

fn parse_provider_float(value: &str, field: ConfigField) -> Result<Option<f64>, String> {
    let parsed = parse_number::<f64>(value)?;
    validate_optional_provider_float(field.label(), parsed)?;
    Ok(parsed)
}

fn option_value<T: ToString>(value: Option<T>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{ConfigField, ConfigFieldValueType, build_resolved_config_from_key_values};
    use crate::config::ResolvedConfig;

    #[test]
    fn descriptor_inventory_has_one_stable_entry_per_field() {
        assert_eq!(ConfigField::ALL.len(), 44);
        let mut keys = HashSet::new();
        for field in ConfigField::ALL {
            let descriptor = field.descriptor();
            assert_eq!(descriptor.key(), field.label());
            assert!(keys.insert(descriptor.key()));
            assert_eq!(descriptor.required(), !field.allows_empty_complete_value());
        }
    }

    #[test]
    fn descriptor_exposes_canonical_editor_shapes_and_bounds() {
        let context = ConfigField::ContextWindow.descriptor();
        assert_eq!(context.value_type(), ConfigFieldValueType::Integer);
        assert_eq!(context.integer_min(), Some(1));
        assert_eq!(context.integer_max(), Some(u32::MAX as u64));

        let parallel = ConfigField::MaxParallelPredictions.descriptor();
        assert_eq!(parallel.integer_min(), Some(1));
        assert_eq!(parallel.integer_max(), Some(u32::MAX as u64));

        for field in [
            ConfigField::MultiAgentMaxAgents,
            ConfigField::MultiAgentMaxModelRequests,
        ] {
            assert_eq!(field.descriptor().integer_min(), Some(1));
        }

        let timeout = ConfigField::RequestTimeoutMs.descriptor();
        assert_eq!(timeout.integer_min(), Some(1));
        assert_eq!(timeout.integer_max(), Some(3_600_000));

        let access = ConfigField::AccessMode.descriptor();
        assert_eq!(access.value_type(), ConfigFieldValueType::Enum);
        assert_eq!(access.options(), &["default", "auto_review", "full_access"]);

        let profile = ConfigField::ProviderProfile.descriptor();
        assert_eq!(profile.value_type(), ConfigFieldValueType::Enum);
        assert_eq!(
            profile.options(),
            &[
                "lm_studio",
                "openai_compatible",
                "openai_responses",
                "lm_studio_chat_completions",
            ]
        );
        assert!(!ConfigField::ApiKeyEnv.descriptor().required());
    }

    #[test]
    fn every_integer_descriptor_bound_is_enforced_by_the_neutral_builder() {
        let base = ResolvedConfig::default();
        for field in ConfigField::ALL {
            let descriptor = field.descriptor();
            if descriptor.value_type() != ConfigFieldValueType::Integer {
                continue;
            }
            if let Some(minimum) = descriptor.integer_min() {
                build_resolved_config_from_key_values(
                    &base,
                    vec![(descriptor.key().to_string(), minimum.to_string())],
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "{} rejected its declared minimum: {error}",
                        descriptor.key()
                    )
                });
                if let Some(below_minimum) = minimum.checked_sub(1) {
                    let error = build_resolved_config_from_key_values(
                        &base,
                        vec![(descriptor.key().to_string(), below_minimum.to_string())],
                    )
                    .unwrap_err();
                    assert!(
                        error.contains(descriptor.key()),
                        "{} lower-bound error omitted the field key: {error}",
                        descriptor.key(),
                    );
                }
            }
            if let Some(maximum) = descriptor.integer_max() {
                build_resolved_config_from_key_values(
                    &base,
                    vec![(descriptor.key().to_string(), maximum.to_string())],
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "{} rejected its declared maximum: {error}",
                        descriptor.key()
                    )
                });
                if let Some(above_maximum) = maximum.checked_add(1) {
                    let error = build_resolved_config_from_key_values(
                        &base,
                        vec![(descriptor.key().to_string(), above_maximum.to_string())],
                    )
                    .unwrap_err();
                    assert!(
                        error.contains(descriptor.key()),
                        "{} upper-bound error omitted the field key: {error}",
                        descriptor.key(),
                    );
                }
            }
        }
    }

    #[test]
    fn optional_provider_numbers_reject_non_finite_values() {
        let base = ResolvedConfig::default();
        for field in [
            ConfigField::Temperature,
            ConfigField::TopP,
            ConfigField::PresencePenalty,
            ConfigField::FrequencyPenalty,
        ] {
            for value in ["NaN", "inf", "-inf"] {
                let error = build_resolved_config_from_key_values(
                    &base,
                    vec![(field.label().to_string(), value.to_string())],
                )
                .unwrap_err();
                assert_eq!(
                    error,
                    format!("config field `{}` must be finite", field.label())
                );
            }
        }
    }

    #[test]
    fn neutral_builder_preserves_stable_key_and_complete_empty_contract() {
        let base = ResolvedConfig::default();
        let changed = build_resolved_config_from_key_values(
            &base,
            vec![(
                ConfigField::Model.label().to_string(),
                "next-model".to_string(),
            )],
        )
        .expect("valid stable-key update");
        assert_eq!(changed.model.model, "next-model");

        let openai_compatible = build_resolved_config_from_key_values(
            &base,
            vec![
                (
                    ConfigField::ProviderProfile.label().to_string(),
                    "openai_compatible".to_string(),
                ),
                (
                    ConfigField::ApiKeyEnv.label().to_string(),
                    "OPENAI_API_KEY".to_string(),
                ),
            ],
        )
        .expect("valid atomic provider profile update");
        assert_eq!(
            openai_compatible.model.provider_profile,
            crate::config::ProviderProfile::OpenAiCompatible
        );
        assert_eq!(
            openai_compatible.model.api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );

        let error = build_resolved_config_from_key_values(
            &base,
            vec![(ConfigField::Model.label().to_string(), String::new())],
        )
        .expect_err("required model must stay non-empty");
        assert!(error.contains("model.model must not be empty"));

        let duplicate = build_resolved_config_from_key_values(
            &base,
            vec![
                (ConfigField::Model.label().to_string(), "first".to_string()),
                (ConfigField::Model.label().to_string(), "second".to_string()),
            ],
        )
        .expect_err("duplicate stable keys must remain invalid");
        assert_eq!(duplicate, "duplicate config field key: model.model");

        let unknown = build_resolved_config_from_key_values(
            &base,
            vec![("model.unknown".to_string(), "value".to_string())],
        )
        .expect_err("unknown stable keys must remain invalid");
        assert_eq!(unknown, "unknown config field key: model.unknown");
    }

    #[test]
    fn enabled_docling_uses_the_neutral_http_endpoint_validator() {
        let base = ResolvedConfig::default();
        let resolved = build_resolved_config_from_key_values(
            &base,
            vec![
                (
                    ConfigField::DoclingEnabled.label().to_string(),
                    "true".to_string(),
                ),
                (
                    ConfigField::DoclingBaseUrl.label().to_string(),
                    " https://docling.example.test/api/ ".to_string(),
                ),
            ],
        )
        .expect("valid Docling endpoint");
        assert_eq!(
            resolved.docling.base_url,
            "https://docling.example.test/api"
        );

        for invalid in [
            "not-an-absolute-url",
            "file:///tmp/docling.sock",
            "https://user:super-secret@docling.example.test",
            "https://docling.example.test?api_key=hidden",
            "https://docling.example.test#hidden",
        ] {
            let error = build_resolved_config_from_key_values(
                &base,
                vec![
                    (
                        ConfigField::DoclingEnabled.label().to_string(),
                        "true".to_string(),
                    ),
                    (
                        ConfigField::DoclingBaseUrl.label().to_string(),
                        invalid.to_string(),
                    ),
                ],
            )
            .expect_err("invalid enabled Docling endpoint");
            assert!(error.contains("docling.base_url"), "{error}");
            assert!(!error.contains("super-secret"));
            assert!(!error.contains("hidden"));
            assert!(!error.contains(invalid));
        }

        let disabled = build_resolved_config_from_key_values(
            &base,
            vec![(
                ConfigField::DoclingBaseUrl.label().to_string(),
                "not-used-while-disabled".to_string(),
            )],
        )
        .expect("disabled Docling preserves an inactive draft value");
        assert!(!disabled.docling.enabled);
        assert_eq!(disabled.docling.base_url, "not-used-while-disabled");
    }
}
