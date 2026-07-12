use std::fs;
use std::io::Write;

use camino::Utf8Path;
use tempfile::NamedTempFile;

use crate::config::loader::global_config_path;
use crate::config::model::{
    AccessMode, McpServerConfig, MultiAgentMode, PartialDoclingConfig, PartialFileGuardConfig,
    PartialInspectionConfig, PartialMcpConfig, PartialModelConfig, PartialMultiAgentConfig,
    PartialPermissionsConfig, PartialResolvedConfig, PartialSessionConfig, PartialShellConfig,
    PromptProfile, ProviderMetadataMode, ResolvedConfig,
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

impl ConfigField {
    pub const ALL: [ConfigField; 47] = [
        ConfigField::BaseUrl,
        ConfigField::Model,
        ConfigField::PromptProfile,
        ConfigField::ProviderMetadataMode,
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
            ConfigField::PromptProfile => "model.prompt_profile",
            ConfigField::ProviderMetadataMode => "model.provider_metadata_mode",
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
            ConfigField::PromptProfile => Some("MOYAI_PROMPT_PROFILE"),
            ConfigField::ProviderMetadataMode => Some("MOYAI_PROVIDER_METADATA_MODE"),
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

    fn toml_path(self) -> (&'static str, &'static str) {
        match self {
            ConfigField::ExtraHeadersJson => ("model", "extra_headers"),
            ConfigField::DoclingHeadersJson => ("docling", "headers"),
            ConfigField::McpServersJson => ("mcp", "servers"),
            _ => self
                .label()
                .split_once('.')
                .expect("config editor labels are section-qualified"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigFieldState {
    pub key: ConfigField,
    pub value: String,
    pub dirty: bool,
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
                    dirty: false,
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
        self.fields[self.selected].dirty = true;
    }

    pub fn backspace(&mut self) {
        self.fields[self.selected].value.pop();
        self.fields[self.selected].dirty = true;
    }

    pub fn clear_selected(&mut self) {
        self.fields[self.selected].value.clear();
        self.fields[self.selected].dirty = true;
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
    let dirty_fields = editor
        .fields
        .iter()
        .filter(|field| field.dirty)
        .map(|field| field.key)
        .collect::<Vec<_>>();
    if dirty_fields.is_empty() {
        return Ok(());
    }

    let mut existing = read_toml_document(path)?;
    let patch = parse_editor_patch_matching(editor, true)?;
    let patch = toml::Value::try_from(patch).map_err(|error| error.to_string())?;
    for field in dirty_fields {
        apply_dirty_toml_field(&mut existing, &patch, field)?;
    }
    let text = toml::to_string_pretty(&existing).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    persist_config_tempfile(path, &text)
}

fn read_toml_document(path: &Utf8Path) -> Result<toml::Value, String> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    if text.trim().is_empty() {
        Ok(toml::Value::Table(toml::map::Map::new()))
    } else {
        toml::from_str(&text).map_err(|error| error.to_string())
    }
}

fn apply_dirty_toml_field(
    existing: &mut toml::Value,
    patch: &toml::Value,
    field: ConfigField,
) -> Result<(), String> {
    let (section_name, field_name) = field.toml_path();
    let patch_value = patch
        .get(section_name)
        .and_then(|section| section.get(field_name))
        .cloned();
    let root = existing
        .as_table_mut()
        .ok_or_else(|| "global config root must be a TOML table".to_string())?;

    if let Some(value) = patch_value {
        let section = root
            .entry(section_name.to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let section = section.as_table_mut().ok_or_else(|| {
            format!("global config section `{section_name}` must be a TOML table")
        })?;
        section.insert(field_name.to_string(), value);
    } else if let Some(section) = root.get_mut(section_name) {
        let section = section.as_table_mut().ok_or_else(|| {
            format!("global config section `{section_name}` must be a TOML table")
        })?;
        section.remove(field_name);
    }
    Ok(())
}

fn persist_config_tempfile(path: &Utf8Path, text: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("config path `{path}` has no parent directory"))?;
    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|error| error.to_string())?;
    temp.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(path.as_std_path())
        .map(|_| ())
        .map_err(|error| error.error.to_string())
}

fn parse_editor_patch(editor: &ConfigEditorState) -> Result<PartialResolvedConfig, String> {
    parse_editor_patch_matching(editor, false)
}

fn parse_editor_patch_matching(
    editor: &ConfigEditorState,
    dirty_only: bool,
) -> Result<PartialResolvedConfig, String> {
    let mut patch = PartialResolvedConfig::default();
    let mut model = PartialModelConfig::default();
    let mut permissions = PartialPermissionsConfig::default();
    let mut multi_agent = PartialMultiAgentConfig::default();
    let mut session = PartialSessionConfig::default();
    let mut shell = PartialShellConfig::default();
    let mut inspection = PartialInspectionConfig::default();
    let mut file_guard = PartialFileGuardConfig::default();
    let mut docling = PartialDoclingConfig::default();
    let mut mcp = PartialMcpConfig::default();

    for field in &editor.fields {
        if dirty_only && !field.dirty {
            continue;
        }
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
            ConfigField::MultiAgentEnabled => multi_agent.enabled = parse_bool(text)?,
            ConfigField::MultiAgentMode => {
                multi_agent.mode = match parse_string(text) {
                    Some(value) => Some(parse_multi_agent_mode(&value)?),
                    None => None,
                }
            }
            ConfigField::MultiAgentMaxAgents => {
                multi_agent.max_concurrent_agents = parse_number(text)?
            }
            ConfigField::MultiAgentMaxModelRequests => {
                multi_agent.max_concurrent_model_requests = parse_number(text)?
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
            ConfigField::ShellHideWindows => shell.hide_windows = parse_bool(text)?,
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
    patch.multi_agent = Some(multi_agent);
    patch.session = Some(session);
    patch.shell = Some(shell);
    patch.inspection = Some(inspection);
    patch.file_guard = Some(file_guard);
    patch.docling = Some(docling);
    patch.mcp = Some(mcp);
    Ok(patch)
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
        ConfigField::MultiAgentEnabled => config.multi_agent.enabled.to_string(),
        ConfigField::MultiAgentMode => config.multi_agent.mode.as_str().to_string(),
        ConfigField::MultiAgentMaxAgents => config.multi_agent.max_concurrent_agents.to_string(),
        ConfigField::MultiAgentMaxModelRequests => {
            config.multi_agent.max_concurrent_model_requests.to_string()
        }
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
        ConfigField::ShellHideWindows => config.shell.hide_windows.to_string(),
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
    use camino::Utf8PathBuf;

    use super::{ConfigEditorState, ConfigField, parse_editor_patch, save_config_sections};
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

    #[test]
    fn config_editor_projects_shell_hide_windows_patch() {
        let mut config = ResolvedConfig::default();
        config.shell.hide_windows = true;
        let mut editor = ConfigEditorState::from_config(&config);
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ShellHideWindows)
            .expect("shell hide window field is present");
        field.value = "false".to_string();

        let patch = parse_editor_patch(&editor).expect("shell hide_windows parses");

        assert_eq!(
            patch.shell.and_then(|shell| shell.hide_windows),
            Some(false)
        );
    }

    #[test]
    fn config_editor_global_save_preserves_unsupported_shell_fields() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[shell]\nprogram = \"pwsh\"\ndefault_timeout_ms = 777\nhide_windows = true\n",
        )
        .expect("seed existing config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let hide_windows = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ShellHideWindows)
            .expect("shell hide_windows field");
        hide_windows.value = "false".to_string();
        hide_windows.dirty = true;

        save_config_sections(&path, &editor).expect("save shell field");
        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(saved["shell"]["program"].as_str(), Some("pwsh"));
        assert_eq!(saved["shell"]["default_timeout_ms"].as_integer(), Some(777));
        assert_eq!(saved["shell"]["hide_windows"].as_bool(), Some(false));
    }

    #[test]
    fn global_save_merges_only_dirty_fields_into_current_toml() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let mut effective = ResolvedConfig::default();
        effective.model.base_url = "http://effective-env-value".to_string();
        effective.model.model = "effective-default".to_string();
        let mut editor = ConfigEditorState::from_config(&effective);

        std::fs::write(
            &path,
            "[model]\nmodel = \"external-current\"\napi_key_env = \"EXTERNAL_KEY\"\n\n[format]\nensure_trailing_newline = false\n\n[future]\nflag = \"keep\"\n",
        )
        .expect("external current config");
        let access = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::AccessMode)
            .expect("access mode field");
        access.value = "full_access".to_string();
        access.dirty = true;

        save_config_sections(&path, &editor).expect("merge dirty config");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(saved["model"]["model"].as_str(), Some("external-current"));
        assert_eq!(saved["model"]["api_key_env"].as_str(), Some("EXTERNAL_KEY"));
        assert!(saved["model"].get("base_url").is_none());
        assert_eq!(
            saved["format"]["ensure_trailing_newline"].as_bool(),
            Some(false)
        );
        assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
    }

    #[test]
    fn global_save_without_dirty_fields_does_not_rewrite_or_pin_effective_values() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let original = "# keep formatting exactly\n[model]\nmodel='current'\n";
        std::fs::write(&path, original).expect("seed config");
        let mut effective = ResolvedConfig::default();
        effective.model.base_url = "http://env-only".to_string();
        let editor = ConfigEditorState::from_config(&effective);

        save_config_sections(&path, &editor).expect("no-op save");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read config"),
            original
        );
    }

    #[test]
    fn clearing_dirty_optional_field_removes_only_that_override() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\ntemperature = 0.7\nmodel = \"keep-model\"\n",
        )
        .expect("seed config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let temperature = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::Temperature)
            .expect("temperature field");
        temperature.value.clear();
        temperature.dirty = true;

        save_config_sections(&path, &editor).expect("clear temperature override");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert!(saved["model"].get("temperature").is_none());
        assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
    }
}
