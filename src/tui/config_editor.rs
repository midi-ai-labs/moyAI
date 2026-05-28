use std::fs;

use camino::Utf8Path;

use crate::config::loader::global_config_path;
use crate::config::model::{
    AccessMode, McpServerConfig, PartialDoclingConfig, PartialFileGuardConfig,
    PartialInspectionConfig, PartialMcpConfig, PartialModelConfig, PartialPermissionsConfig,
    PartialResolvedConfig, PartialSessionConfig, PromptProfile, ProviderMetadataMode,
    ResolvedConfig,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSaveScope {
    Session,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigField {
    BaseUrl,
    Model,
    PromptProfile,
    ProviderMetadataMode,
    AccessMode,
    Temperature,
    TopP,
    TopK,
    PresencePenalty,
    FrequencyPenalty,
    Seed,
    StopSequences,
    ContextWindow,
    MaxOutputTokens,
    SessionMaxStepsPerTurn,
    RequestTimeoutMs,
    StreamIdleTimeoutMs,
    ConnectTimeoutMs,
    MaxRetries,
    StreamMaxRetries,
    SupportsTools,
    SupportsReasoning,
    SupportsImages,
    ParallelToolCalls,
    MaxParallelPredictions,
    ExtraHeadersJson,
    ExtraBodyJson,
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

impl ConfigField {
    pub const ALL: [ConfigField; 42] = [
        ConfigField::BaseUrl,
        ConfigField::Model,
        ConfigField::PromptProfile,
        ConfigField::ProviderMetadataMode,
        ConfigField::AccessMode,
        ConfigField::Temperature,
        ConfigField::TopP,
        ConfigField::TopK,
        ConfigField::PresencePenalty,
        ConfigField::FrequencyPenalty,
        ConfigField::Seed,
        ConfigField::StopSequences,
        ConfigField::ContextWindow,
        ConfigField::MaxOutputTokens,
        ConfigField::SessionMaxStepsPerTurn,
        ConfigField::RequestTimeoutMs,
        ConfigField::StreamIdleTimeoutMs,
        ConfigField::ConnectTimeoutMs,
        ConfigField::MaxRetries,
        ConfigField::StreamMaxRetries,
        ConfigField::SupportsTools,
        ConfigField::SupportsReasoning,
        ConfigField::SupportsImages,
        ConfigField::ParallelToolCalls,
        ConfigField::MaxParallelPredictions,
        ConfigField::ExtraHeadersJson,
        ConfigField::ExtraBodyJson,
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
            ConfigField::PromptProfile => "model.prompt_profile",
            ConfigField::ProviderMetadataMode => "model.provider_metadata_mode",
            ConfigField::AccessMode => "permissions.access_mode",
            ConfigField::Temperature => "model.temperature",
            ConfigField::TopP => "model.top_p",
            ConfigField::TopK => "model.top_k",
            ConfigField::PresencePenalty => "model.presence_penalty",
            ConfigField::FrequencyPenalty => "model.frequency_penalty",
            ConfigField::Seed => "model.seed",
            ConfigField::StopSequences => "model.stop_sequences",
            ConfigField::ContextWindow => "model.context_window",
            ConfigField::MaxOutputTokens => "model.max_output_tokens",
            ConfigField::SessionMaxStepsPerTurn => "session.max_steps_per_turn",
            ConfigField::RequestTimeoutMs => "model.request_timeout_ms",
            ConfigField::StreamIdleTimeoutMs => "model.stream_idle_timeout_ms",
            ConfigField::ConnectTimeoutMs => "model.connect_timeout_ms",
            ConfigField::MaxRetries => "model.max_retries",
            ConfigField::StreamMaxRetries => "model.stream_max_retries",
            ConfigField::SupportsTools => "model.supports_tools",
            ConfigField::SupportsReasoning => "model.supports_reasoning",
            ConfigField::SupportsImages => "model.supports_images",
            ConfigField::ParallelToolCalls => "model.parallel_tool_calls",
            ConfigField::MaxParallelPredictions => "model.max_parallel_predictions",
            ConfigField::ExtraHeadersJson => "model.extra_headers_json",
            ConfigField::ExtraBodyJson => "model.extra_body_json",
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
            ConfigField::PromptProfile => Some("MOYAI_PROMPT_PROFILE"),
            ConfigField::ProviderMetadataMode => Some("MOYAI_PROVIDER_METADATA_MODE"),
            ConfigField::AccessMode => Some("MOYAI_ACCESS_MODE"),
            ConfigField::Temperature => Some("MOYAI_TEMPERATURE"),
            ConfigField::TopP => Some("MOYAI_TOP_P"),
            ConfigField::TopK => Some("MOYAI_TOP_K"),
            ConfigField::PresencePenalty => Some("MOYAI_PRESENCE_PENALTY"),
            ConfigField::FrequencyPenalty => Some("MOYAI_FREQUENCY_PENALTY"),
            ConfigField::Seed => Some("MOYAI_SEED"),
            ConfigField::StopSequences => Some("MOYAI_STOP_SEQUENCES"),
            ConfigField::ContextWindow => Some("MOYAI_CONTEXT_WINDOW"),
            ConfigField::MaxOutputTokens => Some("MOYAI_MAX_OUTPUT_TOKENS"),
            ConfigField::SessionMaxStepsPerTurn => Some("MOYAI_MAX_STEPS_PER_TURN"),
            ConfigField::RequestTimeoutMs => Some("MOYAI_REQUEST_TIMEOUT_MS"),
            ConfigField::StreamIdleTimeoutMs => Some("MOYAI_STREAM_IDLE_TIMEOUT_MS"),
            ConfigField::ConnectTimeoutMs => Some("MOYAI_CONNECT_TIMEOUT_MS"),
            ConfigField::MaxRetries => Some("MOYAI_MAX_RETRIES"),
            ConfigField::StreamMaxRetries => Some("MOYAI_STREAM_MAX_RETRIES"),
            ConfigField::SupportsTools => Some("MOYAI_SUPPORTS_TOOLS"),
            ConfigField::SupportsReasoning => Some("MOYAI_SUPPORTS_REASONING"),
            ConfigField::SupportsImages => Some("MOYAI_SUPPORTS_IMAGES"),
            ConfigField::ParallelToolCalls => Some("MOYAI_PARALLEL_TOOL_CALLS"),
            ConfigField::MaxParallelPredictions => Some("MOYAI_MAX_PARALLEL_PREDICTIONS"),
            ConfigField::ExtraHeadersJson => Some("MOYAI_EXTRA_HEADERS"),
            ConfigField::ExtraBodyJson => Some("MOYAI_EXTRA_BODY_JSON"),
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
}

#[derive(Debug, Clone)]
pub struct ConfigFieldState {
    pub key: ConfigField,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct ConfigEditorState {
    pub fields: Vec<ConfigFieldState>,
    pub selected: usize,
    pub feedback: Option<String>,
}

impl ConfigEditorState {
    pub fn from_config(config: &ResolvedConfig) -> Self {
        Self {
            fields: ConfigField::ALL
                .into_iter()
                .map(|key| ConfigFieldState {
                    key,
                    value: field_value(key, config),
                })
                .collect(),
            selected: 0,
            feedback: None,
        }
    }

    pub fn selected_field(&self) -> &ConfigFieldState {
        &self.fields[self.selected]
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.fields.len();
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, len as isize - 1);
        self.selected = next as usize;
    }

    pub fn insert_char(&mut self, value: char) {
        self.fields[self.selected].value.push(value);
    }

    pub fn backspace(&mut self) {
        self.fields[self.selected].value.pop();
    }

    pub fn clear_selected(&mut self) {
        self.fields[self.selected].value.clear();
    }

    pub fn build_session_override(&self) -> Result<PartialResolvedConfig, String> {
        parse_editor_patch(self)
    }

    pub fn save_scope(&self, _root: &Utf8Path, scope: ConfigSaveScope) -> Result<String, String> {
        match scope {
            ConfigSaveScope::Session => {
                return Err("session override is memory only; use Apply Session".to_string());
            }
            ConfigSaveScope::Global => {
                let path = global_config_path().map_err(|error| error.to_string())?;
                save_config_sections(&path, self)?;
                Ok(format!("saved global config to {}", path))
            }
        }
    }
}

fn save_config_sections(path: &Utf8Path, editor: &ConfigEditorState) -> Result<(), String> {
    let mut existing = read_partial(path)?;
    let patch = parse_editor_patch(editor)?;
    existing.model = patch.model.filter(|value| !model_patch_is_empty(value));
    existing.permissions = patch
        .permissions
        .filter(|value| !permissions_patch_is_empty(value));
    existing.session = patch.session.filter(|value| !session_patch_is_empty(value));
    existing.inspection = patch
        .inspection
        .filter(|value| !inspection_patch_is_empty(value));
    existing.file_guard = patch
        .file_guard
        .filter(|value| !file_guard_patch_is_empty(value));
    existing.docling = patch.docling.filter(|value| !docling_patch_is_empty(value));
    existing.mcp = patch.mcp.filter(|value| !mcp_patch_is_empty(value));
    write_partial(path, &existing)
}

fn read_partial(path: &Utf8Path) -> Result<PartialResolvedConfig, String> {
    if !path.exists() {
        return Ok(PartialResolvedConfig::default());
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    toml::from_str(&text).map_err(|error| error.to_string())
}

fn write_partial(path: &Utf8Path, patch: &PartialResolvedConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = toml::to_string_pretty(patch).map_err(|error| error.to_string())?;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, text).map_err(|error| error.to_string())?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&temp_path, path).map_err(|error| error.to_string())
}

fn parse_editor_patch(editor: &ConfigEditorState) -> Result<PartialResolvedConfig, String> {
    let mut patch = PartialResolvedConfig::default();
    let mut model = PartialModelConfig::default();
    let mut permissions = PartialPermissionsConfig::default();
    let mut session = PartialSessionConfig::default();
    let mut inspection = PartialInspectionConfig::default();
    let mut file_guard = PartialFileGuardConfig::default();
    let mut docling = PartialDoclingConfig::default();
    let mut mcp = PartialMcpConfig::default();

    for field in &editor.fields {
        let text = field.value.trim();
        match field.key {
            ConfigField::BaseUrl => model.base_url = parse_string(text),
            ConfigField::Model => model.model = parse_string(text),
            ConfigField::PromptProfile => {
                model.prompt_profile = match parse_string(text) {
                    Some(value) => Some(parse_prompt_profile(&value)?),
                    None => None,
                }
            }
            ConfigField::ProviderMetadataMode => {
                model.provider_metadata_mode = match parse_string(text) {
                    Some(value) => Some(parse_provider_metadata_mode(&value)?),
                    None => None,
                }
            }
            ConfigField::AccessMode => {
                permissions.access_mode = match parse_string(text) {
                    Some(value) => Some(parse_access_mode(&value)?),
                    None => None,
                }
            }
            ConfigField::Temperature => model.temperature = parse_number(text)?,
            ConfigField::TopP => model.top_p = parse_number(text)?,
            ConfigField::TopK => model.top_k = parse_number(text)?,
            ConfigField::PresencePenalty => model.presence_penalty = parse_number(text)?,
            ConfigField::FrequencyPenalty => model.frequency_penalty = parse_number(text)?,
            ConfigField::Seed => model.seed = parse_number(text)?,
            ConfigField::StopSequences => model.stop_sequences = Some(parse_csv(text)),
            ConfigField::ContextWindow => model.context_window = parse_number(text)?,
            ConfigField::MaxOutputTokens => model.max_output_tokens = parse_number(text)?,
            ConfigField::SessionMaxStepsPerTurn => session.max_steps_per_turn = parse_number(text)?,
            ConfigField::RequestTimeoutMs => model.request_timeout_ms = parse_number(text)?,
            ConfigField::StreamIdleTimeoutMs => model.stream_idle_timeout_ms = parse_number(text)?,
            ConfigField::ConnectTimeoutMs => model.connect_timeout_ms = parse_number(text)?,
            ConfigField::MaxRetries => model.max_retries = parse_number(text)?,
            ConfigField::StreamMaxRetries => model.stream_max_retries = parse_number(text)?,
            ConfigField::SupportsTools => model.supports_tools = parse_bool(text)?,
            ConfigField::SupportsReasoning => model.supports_reasoning = parse_bool(text)?,
            ConfigField::SupportsImages => model.supports_images = parse_bool(text)?,
            ConfigField::ParallelToolCalls => model.parallel_tool_calls = parse_bool(text)?,
            ConfigField::MaxParallelPredictions => {
                model.max_parallel_predictions = parse_number(text)?
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
            ConfigField::InspectionDefaultMaxDepth => {
                inspection.default_max_depth = parse_number(text)?
            }
            ConfigField::InspectionDefaultMaxEntriesPerDir => {
                inspection.default_max_entries_per_dir = parse_number(text)?
            }
            ConfigField::InspectionMaxExtensionsReported => {
                inspection.max_extensions_reported = parse_number(text)?
            }
            ConfigField::InspectionIncludeHiddenByDefault => {
                inspection.include_hidden_by_default = parse_bool(text)?
            }
            ConfigField::FileGuardMaxInlineReadBytes => {
                file_guard.max_inline_read_bytes = parse_number(text)?
            }
            ConfigField::FileGuardLargeFileWarningBytes => {
                file_guard.large_file_warning_bytes = parse_number(text)?
            }
            ConfigField::FileGuardBlockedReadExtensions => {
                file_guard.blocked_read_extensions = Some(parse_extension_csv(text))
            }
            ConfigField::FileGuardStructuredDocumentExtensions => {
                file_guard.structured_document_extensions = Some(parse_extension_csv(text))
            }
            ConfigField::DoclingEnabled => docling.enabled = parse_bool(text)?,
            ConfigField::DoclingBaseUrl => docling.base_url = parse_string(text),
            ConfigField::DoclingTimeoutMs => docling.timeout_ms = parse_number(text)?,
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
    patch.session = Some(session);
    patch.inspection = Some(inspection);
    patch.file_guard = Some(file_guard);
    patch.docling = Some(docling);
    patch.mcp = Some(mcp);
    Ok(patch)
}

fn model_patch_is_empty(model: &PartialModelConfig) -> bool {
    model.base_url.is_none()
        && model.model.is_none()
        && model.prompt_profile.is_none()
        && model.provider_metadata_mode.is_none()
        && model.api_key_env.is_none()
        && model.extra_headers.is_none()
        && model.request_timeout_ms.is_none()
        && model.stream_idle_timeout_ms.is_none()
        && model.connect_timeout_ms.is_none()
        && model.max_retries.is_none()
        && model.stream_max_retries.is_none()
        && model.context_window.is_none()
        && model.max_output_tokens.is_none()
        && model.temperature.is_none()
        && model.top_p.is_none()
        && model.top_k.is_none()
        && model.presence_penalty.is_none()
        && model.frequency_penalty.is_none()
        && model.seed.is_none()
        && model.stop_sequences.is_none()
        && model.supports_tools.is_none()
        && model.supports_reasoning.is_none()
        && model.supports_images.is_none()
        && model.parallel_tool_calls.is_none()
        && model.max_parallel_predictions.is_none()
        && model.extra_body_json.is_none()
}

fn permissions_patch_is_empty(permissions: &PartialPermissionsConfig) -> bool {
    permissions.access_mode.is_none()
        && permissions.additional_read_roots.is_none()
        && permissions.additional_write_roots.is_none()
}

fn session_patch_is_empty(session: &PartialSessionConfig) -> bool {
    session.default_title_max_len.is_none()
        && session.transcript_limit_messages.is_none()
        && session.auto_resume_last.is_none()
        && session.max_steps_per_turn.is_none()
        && session.overflow_margin_tokens.is_none()
}

fn inspection_patch_is_empty(patch: &PartialInspectionConfig) -> bool {
    patch.default_max_depth.is_none()
        && patch.default_max_entries_per_dir.is_none()
        && patch.max_extensions_reported.is_none()
        && patch.include_hidden_by_default.is_none()
}

fn file_guard_patch_is_empty(patch: &PartialFileGuardConfig) -> bool {
    patch.max_inline_read_bytes.is_none()
        && patch.large_file_warning_bytes.is_none()
        && patch.blocked_read_extensions.is_none()
        && patch.structured_document_extensions.is_none()
}

fn docling_patch_is_empty(patch: &PartialDoclingConfig) -> bool {
    patch.enabled.is_none()
        && patch.base_url.is_none()
        && patch.timeout_ms.is_none()
        && patch.api_key_env.is_none()
        && patch.headers.is_none()
}

fn mcp_patch_is_empty(patch: &PartialMcpConfig) -> bool {
    patch.enabled.is_none() && patch.servers.is_none()
}

fn parse_prompt_profile(value: &str) -> Result<PromptProfile, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(PromptProfile::Auto),
        "default" => Ok(PromptProfile::Default),
        "qwen" | "qwen_coder" | "qwen-coder" => Ok(PromptProfile::QwenCoder),
        other => Err(format!("unsupported prompt_profile `{other}`")),
    }
}

fn parse_provider_metadata_mode(value: &str) -> Result<ProviderMetadataMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "lm_studio_native_required"
        | "lm-studio-native-required"
        | "lmstudio"
        | "lm_studio"
        | "lm-studio" => Ok(ProviderMetadataMode::LmStudioNativeRequired),
        "openai_compatible_only"
        | "openai-compatible-only"
        | "openai"
        | "openai_compat"
        | "openai-compatible" => Ok(ProviderMetadataMode::OpenAiCompatibleOnly),
        other => Err(format!("unsupported provider_metadata_mode `{other}`")),
    }
}

fn parse_access_mode(value: &str) -> Result<AccessMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "default" | "normal" => Ok(AccessMode::Default),
        "auto_review" | "auto-review" | "autoreview" | "auto" => Ok(AccessMode::AutoReview),
        "full_access" | "full-access" | "full" => Ok(AccessMode::FullAccess),
        other => Err(format!("unsupported access_mode `{other}`")),
    }
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

fn field_value(key: ConfigField, config: &ResolvedConfig) -> String {
    match key {
        ConfigField::BaseUrl => config.model.base_url.clone(),
        ConfigField::Model => config.model.model.clone(),
        ConfigField::PromptProfile => match config.model.prompt_profile {
            PromptProfile::Auto => "auto".to_string(),
            PromptProfile::Default => "default".to_string(),
            PromptProfile::QwenCoder => "qwen_coder".to_string(),
        },
        ConfigField::ProviderMetadataMode => match config.model.provider_metadata_mode {
            ProviderMetadataMode::LmStudioNativeRequired => "lm_studio_native_required".to_string(),
            ProviderMetadataMode::OpenAiCompatibleOnly => "openai_compatible_only".to_string(),
        },
        ConfigField::AccessMode => match config.permissions.access_mode {
            AccessMode::Default => "default".to_string(),
            AccessMode::AutoReview => "auto_review".to_string(),
            AccessMode::FullAccess => "full_access".to_string(),
        },
        ConfigField::Temperature => config
            .model
            .temperature
            .map(|value| value.to_string())
            .unwrap_or_default(),
        ConfigField::TopP => config
            .model
            .top_p
            .map(|value| value.to_string())
            .unwrap_or_default(),
        ConfigField::TopK => config
            .model
            .top_k
            .map(|value| value.to_string())
            .unwrap_or_default(),
        ConfigField::PresencePenalty => config
            .model
            .presence_penalty
            .map(|value| value.to_string())
            .unwrap_or_default(),
        ConfigField::FrequencyPenalty => config
            .model
            .frequency_penalty
            .map(|value| value.to_string())
            .unwrap_or_default(),
        ConfigField::Seed => config
            .model
            .seed
            .map(|value| value.to_string())
            .unwrap_or_default(),
        ConfigField::StopSequences => config.model.stop_sequences.join(", "),
        ConfigField::ContextWindow => config.model.context_window.to_string(),
        ConfigField::MaxOutputTokens => config.model.max_output_tokens.to_string(),
        ConfigField::SessionMaxStepsPerTurn => config.session.max_steps_per_turn.to_string(),
        ConfigField::RequestTimeoutMs => config.model.request_timeout_ms.to_string(),
        ConfigField::StreamIdleTimeoutMs => config.model.stream_idle_timeout_ms.to_string(),
        ConfigField::ConnectTimeoutMs => config.model.connect_timeout_ms.to_string(),
        ConfigField::MaxRetries => config.model.max_retries.to_string(),
        ConfigField::StreamMaxRetries => config.model.stream_max_retries.to_string(),
        ConfigField::SupportsTools => config.model.supports_tools.to_string(),
        ConfigField::SupportsReasoning => config.model.supports_reasoning.to_string(),
        ConfigField::SupportsImages => config.model.supports_images.to_string(),
        ConfigField::ParallelToolCalls => config.model.parallel_tool_calls.to_string(),
        ConfigField::MaxParallelPredictions => config.model.max_parallel_predictions.to_string(),
        ConfigField::ExtraHeadersJson => {
            serde_json::to_string(&config.model.extra_headers).unwrap_or_default()
        }
        ConfigField::ExtraBodyJson => config
            .model
            .extra_body_json
            .as_ref()
            .map(ValueExt::to_json_string)
            .unwrap_or_default(),
        ConfigField::InspectionDefaultMaxDepth => config.inspection.default_max_depth.to_string(),
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

trait ValueExt {
    fn to_json_string(&self) -> String;
}

impl ValueExt for serde_json::Value {
    fn to_json_string(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigEditorState, ConfigField, parse_editor_patch};
    use crate::config::{ProviderMetadataMode, ResolvedConfig};

    #[test]
    fn config_editor_projects_provider_metadata_mode_patch() {
        let config = ResolvedConfig::default();
        let mut editor = ConfigEditorState::from_config(&config);
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ProviderMetadataMode)
            .expect("provider metadata mode field is present");
        field.value = "openai_compatible_only".to_string();

        let patch = parse_editor_patch(&editor).expect("provider mode parses");

        assert_eq!(
            patch.model.and_then(|model| model.provider_metadata_mode),
            Some(ProviderMetadataMode::OpenAiCompatibleOnly)
        );
    }
}
