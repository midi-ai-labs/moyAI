use std::fs;
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::NamedTempFile;

use crate::config::ProviderEndpoint;
use crate::config::loader::{
    acquire_global_config_write_lease, global_config_path, read_toml_utf8_bounded,
};
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::model::{
    AccessMode, McpServerConfig, MultiAgentMode, PartialDoclingConfig, PartialFileGuardConfig,
    PartialInspectionConfig, PartialMcpConfig, PartialModelConfig, PartialMultiAgentConfig,
    PartialPermissionsConfig, PartialResolvedConfig, PartialShellConfig, ProviderMetadataMode,
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
    RequestTimeoutMs,
    StreamIdleTimeoutMs,
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

impl ConfigField {
    pub const ALL: [ConfigField; 44] = [
        ConfigField::BaseUrl,
        ConfigField::Model,
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
        ConfigField::RequestTimeoutMs,
        ConfigField::StreamIdleTimeoutMs,
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
            ConfigField::RequestTimeoutMs => "model.request_timeout_ms",
            ConfigField::StreamIdleTimeoutMs => "model.stream_idle_timeout_ms",
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
            ConfigField::RequestTimeoutMs => Some("MOYAI_REQUEST_TIMEOUT_MS"),
            ConfigField::StreamIdleTimeoutMs => Some("MOYAI_STREAM_IDLE_TIMEOUT_MS"),
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

    pub fn from_config_values(
        config: &ResolvedConfig,
        values: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let mut candidate = Self::from_config(config);
        candidate.replace_values_by_key(values)?;
        Ok(candidate)
    }

    pub fn replace_values_by_key(&mut self, values: Vec<(String, String)>) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        let mut updates = Vec::with_capacity(values.len());
        for (key, value) in values {
            if !seen.insert(key.clone()) {
                return Err(format!("duplicate config field key: {key}"));
            }
            let index = self
                .fields
                .iter()
                .position(|field| field.key.label() == key)
                .ok_or_else(|| format!("unknown config field key: {key}"))?;
            updates.push((index, value));
        }
        for (index, value) in updates {
            let field = &mut self.fields[index];
            field.dirty = field.value != value;
            field.value = value;
        }
        Ok(())
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

    pub fn build_resolved_config(&self, base: &ResolvedConfig) -> Result<ResolvedConfig, String> {
        validate_complete_editor_values(self)?;
        let mut config = apply_config_patch(base.clone(), parse_editor_patch(self)?);

        for field in &self.fields {
            if !field.value.trim().is_empty() {
                continue;
            }
            match field.key {
                ConfigField::Temperature => config.model.temperature = None,
                ConfigField::TopP => config.model.top_p = None,
                ConfigField::TopK => config.model.top_k = None,
                ConfigField::PresencePenalty => config.model.presence_penalty = None,
                ConfigField::FrequencyPenalty => config.model.frequency_penalty = None,
                ConfigField::Seed => config.model.seed = None,
                ConfigField::ExtraHeadersJson => config.model.extra_headers.clear(),
                ConfigField::ExtraBodyJson => config.model.extra_body_json = None,
                ConfigField::DoclingApiKeyEnv => config.docling.api_key_env = None,
                ConfigField::DoclingHeadersJson => config.docling.headers.clear(),
                ConfigField::McpServersJson => config.mcp.servers.clear(),
                _ => {}
            }
        }

        Ok(config)
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

    pub fn remember_global_access_mode(access_mode: AccessMode) -> Result<Utf8PathBuf, String> {
        let path = global_config_path().map_err(|error| error.to_string())?;
        save_access_mode(&path, access_mode)?;
        Ok(path)
    }

    pub fn compare_and_set_global_access_mode(
        expected: AccessMode,
        access_mode: AccessMode,
    ) -> Result<Option<Utf8PathBuf>, String> {
        let path = global_config_path().map_err(|error| error.to_string())?;
        compare_and_set_access_mode(&path, expected, access_mode)
            .map(|updated| updated.then_some(path))
    }
}

fn save_access_mode(path: &Utf8Path, access_mode: AccessMode) -> Result<(), String> {
    write_access_mode(path, None, access_mode).map(|_| ())
}

fn compare_and_set_access_mode(
    path: &Utf8Path,
    expected: AccessMode,
    access_mode: AccessMode,
) -> Result<bool, String> {
    write_access_mode(path, Some(expected), access_mode)
}

fn write_access_mode(
    path: &Utf8Path,
    expected: Option<AccessMode>,
    access_mode: AccessMode,
) -> Result<bool, String> {
    let _write_lease =
        acquire_global_config_write_lease(path).map_err(|error| error.to_string())?;
    let mut existing = read_toml_document(path)?;
    let current = access_mode_from_document(&existing)?;
    if expected.is_some_and(|expected| current != expected) {
        return Ok(false);
    }
    let root = existing
        .as_table_mut()
        .ok_or_else(|| "global config root must be a TOML table".to_string())?;
    let permissions = root
        .entry("permissions".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "global config section `permissions` must be a TOML table".to_string())?;
    permissions.insert(
        "access_mode".to_string(),
        toml::Value::String(access_mode.as_str().to_string()),
    );
    normalize_provider_endpoint_in_document(&mut existing)?;
    let text = toml::to_string_pretty(&existing).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    persist_config_tempfile(path, &text)?;
    Ok(true)
}

fn access_mode_from_document(document: &toml::Value) -> Result<AccessMode, String> {
    let Some(value) = document
        .get("permissions")
        .and_then(|permissions| permissions.get("access_mode"))
    else {
        return Ok(ResolvedConfig::default().permissions.access_mode);
    };
    let value = value
        .as_str()
        .ok_or_else(|| "permissions.access_mode must be a string".to_string())?;
    match value {
        "default" | "standard" => Ok(AccessMode::Default),
        // One-way normalization for user config written by the retired AI-review mode.
        "auto_review" | "auto-review" => Ok(AccessMode::Default),
        "full_access" | "full-access" => Ok(AccessMode::FullAccess),
        _ => Err(format!("unknown permissions.access_mode `{value}`")),
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

    let _write_lease =
        acquire_global_config_write_lease(path).map_err(|error| error.to_string())?;
    let mut existing = read_toml_document(path)?;
    let patch = parse_editor_patch_matching(editor, true)?;
    let patch = toml::Value::try_from(patch).map_err(|error| error.to_string())?;
    for field in dirty_fields {
        apply_dirty_toml_field(&mut existing, &patch, field)?;
    }
    normalize_provider_endpoint_in_document(&mut existing)?;
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
    let text = read_toml_utf8_bounded(path).map_err(|error| error.to_string())?;
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

fn normalize_provider_endpoint_in_document(document: &mut toml::Value) -> Result<(), String> {
    let Some(model) = document.get_mut("model") else {
        return Ok(());
    };
    let model = model
        .as_table_mut()
        .ok_or_else(|| "global config section `model` must be a TOML table".to_string())?;
    let Some(base_url) = model.get_mut("base_url") else {
        return Ok(());
    };
    let raw = base_url
        .as_str()
        .ok_or_else(|| "model.base_url must be a string".to_string())?;
    let endpoint = ProviderEndpoint::parse(raw).map_err(|error| error.to_string())?;
    *base_url = toml::Value::String(endpoint.as_str().to_string());
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

fn validate_complete_editor_values(editor: &ConfigEditorState) -> Result<(), String> {
    for field in &editor.fields {
        if !field.value.trim().is_empty() || field_allows_empty_complete_value(field.key) {
            continue;
        }
        return Err(format!("{} must not be empty", field.key.label()));
    }
    Ok(())
}

fn field_allows_empty_complete_value(field: ConfigField) -> bool {
    matches!(
        field,
        ConfigField::Temperature
            | ConfigField::TopP
            | ConfigField::TopK
            | ConfigField::PresencePenalty
            | ConfigField::FrequencyPenalty
            | ConfigField::Seed
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

fn parse_editor_patch_matching(
    editor: &ConfigEditorState,
    dirty_only: bool,
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

    for field in &editor.fields {
        if dirty_only && !field.dirty {
            continue;
        }
        let text = field.value.trim();
        match field.key {
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
            ConfigField::RequestTimeoutMs => model.request_timeout_ms = parse_number(text)?,
            ConfigField::StreamIdleTimeoutMs => model.stream_idle_timeout_ms = parse_number(text)?,
            ConfigField::ConnectTimeoutMs => model.connect_timeout_ms = parse_number(text)?,
            ConfigField::MaxRetries => model.max_retries = parse_number(text)?,
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
    patch.shell = Some(shell);
    patch.inspection = Some(inspection);
    patch.file_guard = Some(file_guard);
    patch.docling = Some(docling);
    patch.mcp = Some(mcp);
    Ok(patch)
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
        "auto_review" | "auto-review" | "autoreview" | "auto" => Ok(AccessMode::Default),
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
        ConfigField::ProviderMetadataMode => match config.model.provider_metadata_mode {
            ProviderMetadataMode::LmStudioNativeRequired => "lm_studio_native_required".to_string(),
            ProviderMetadataMode::OpenAiCompatibleOnly => "openai_compatible_only".to_string(),
        },
        ConfigField::AccessMode => match config.permissions.access_mode {
            AccessMode::Default => "default".to_string(),
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
        ConfigField::RequestTimeoutMs => config.model.request_timeout_ms.to_string(),
        ConfigField::StreamIdleTimeoutMs => config.model.stream_idle_timeout_ms.to_string(),
        ConfigField::ConnectTimeoutMs => config.model.connect_timeout_ms.to_string(),
        ConfigField::MaxRetries => config.model.max_retries.to_string(),
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
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use camino::Utf8PathBuf;

    use super::{
        ConfigEditorState, ConfigField, compare_and_set_access_mode, parse_editor_patch,
        save_access_mode, save_config_sections,
    };
    use crate::config::{AccessMode, ProviderMetadataMode, ResolvedConfig};

    #[test]
    fn config_editor_excludes_removed_model_behavior_guards() {
        let editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let labels = editor
            .fields
            .iter()
            .map(|field| field.key.label())
            .collect::<Vec<_>>();

        assert!(!labels.contains(&"model.prompt_profile"));
        assert!(!labels.contains(&"session.max_steps_per_turn"));
    }

    #[test]
    fn config_value_candidate_uses_stable_keys_and_rejects_invalid_batch_atomically() {
        let config = ResolvedConfig::default();
        let mut editor = ConfigEditorState::from_config(&config);
        let original_model = editor
            .fields
            .iter()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field")
            .value
            .clone();

        let error = editor
            .replace_values_by_key(vec![
                ("model.model".to_string(), "changed-model".to_string()),
                ("unknown.field".to_string(), "invalid".to_string()),
            ])
            .expect_err("unknown field must reject the full batch");
        assert!(error.contains("unknown config field key"));
        let model = editor
            .fields
            .iter()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        assert_eq!(model.value, original_model);
        assert!(!model.dirty);

        let candidate = ConfigEditorState::from_config_values(
            &config,
            vec![("model.model".to_string(), "changed-model".to_string())],
        )
        .expect("known stable key");
        let model = candidate
            .fields
            .iter()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        assert_eq!(model.value, "changed-model");
        assert!(model.dirty);
    }

    #[test]
    fn complete_session_candidate_preserves_explicit_optional_absence() {
        let mut base = ResolvedConfig::default();
        base.model.temperature = Some(0.7);
        base.model.extra_body_json = Some(serde_json::json!({"num_ctx": 32768}));
        let candidate = ConfigEditorState::from_config_values(
            &base,
            vec![
                (ConfigField::Temperature.label().to_string(), String::new()),
                (
                    ConfigField::ExtraBodyJson.label().to_string(),
                    String::new(),
                ),
            ],
        )
        .expect("complete config values");

        let resolved = candidate
            .build_resolved_config(&base)
            .expect("complete config candidate");

        assert_eq!(resolved.model.temperature, None);
        assert_eq!(resolved.model.extra_body_json, None);
        assert_eq!(resolved.model.model, base.model.model);
    }

    #[test]
    fn complete_session_candidate_rejects_missing_required_values() {
        let base = ResolvedConfig::default();
        let candidate = ConfigEditorState::from_config_values(
            &base,
            vec![(ConfigField::Model.label().to_string(), String::new())],
        )
        .expect("complete config values");

        let error = candidate
            .build_resolved_config(&base)
            .expect_err("required model cannot be cleared");

        assert_eq!(error, "model.model must not be empty");
    }

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
    fn config_editor_canonicalizes_lm_studio_endpoint_and_rejects_url_secrets() {
        let config = ResolvedConfig::default();
        let mut editor = ConfigEditorState::from_config(&config);
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::BaseUrl)
            .expect("provider endpoint field");
        field.value = " http://lm-studio.local:1234/v1/ ".to_string();

        let patch = parse_editor_patch(&editor).expect("valid LM Studio endpoint");
        assert_eq!(
            patch.model.and_then(|model| model.base_url),
            Some("http://lm-studio.local:1234/v1".to_string())
        );

        for raw in [
            "https://user:super-secret@provider.example/v1",
            "https://provider.example/v1?api_key=hidden",
            "https://provider.example/v1#hidden",
        ] {
            let mut editor = ConfigEditorState::from_config(&config);
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == ConfigField::BaseUrl)
                .expect("provider endpoint field");
            field.value = raw.to_string();
            let error = parse_editor_patch(&editor).expect_err("reject secret endpoint");
            assert!(!error.contains("super-secret"));
            assert!(!error.contains("hidden"));
            assert!(!error.contains(raw));
        }
    }

    #[test]
    fn global_config_writes_never_persist_an_invalid_provider_endpoint() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let original = "[model]\nbase_url = \"http://provider.example\"\n";
        std::fs::write(&path, original).expect("seed config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::BaseUrl)
            .expect("provider endpoint field");
        field.value = "https://user:super-secret@provider.example/v1".to_string();
        field.dirty = true;

        let error = save_config_sections(&path, &editor).expect_err("reject invalid endpoint");

        assert!(!error.contains("super-secret"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read config"),
            original
        );

        std::fs::write(
            &path,
            "[model]\nbase_url = \"https://provider.example/v1?api_key=hidden\"\n",
        )
        .expect("seed invalid existing config");
        let error = save_access_mode(&path, AccessMode::FullAccess)
            .expect_err("unrelated save cannot preserve invalid endpoint");
        assert!(!error.contains("hidden"));
        let saved = std::fs::read_to_string(&path).expect("read unchanged config");
        assert!(saved.contains("api_key=hidden"));
        assert!(!saved.contains("full_access"));
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
    fn remembering_access_mode_updates_only_the_existing_permission_field() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\nmodel = \"keep-model\"\n\n[permissions]\naccess_mode = \"default\"\n\n[future]\nflag = \"keep\"\n",
        )
        .expect("seed config");

        for (mode, expected) in [
            (AccessMode::FullAccess, "full_access"),
            (AccessMode::Default, "default"),
        ] {
            save_access_mode(&path, mode).expect("remember access mode");

            let saved = std::fs::read_to_string(&path).expect("read saved config");
            let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
            assert_eq!(saved["permissions"]["access_mode"].as_str(), Some(expected));
            assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
            assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
        }
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

    #[test]
    fn access_mode_compare_and_set_preserves_external_field_changes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[permissions]\naccess_mode = \"default\"\n[model]\nmodel = \"keep-model\"\n",
        )
        .expect("seed config");

        assert!(
            compare_and_set_access_mode(&path, AccessMode::Default, AccessMode::FullAccess)
                .expect("first CAS")
        );
        assert!(
            !compare_and_set_access_mode(&path, AccessMode::Default, AccessMode::Default)
                .expect("stale CAS")
        );

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
        assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
    }

    #[test]
    fn concurrent_global_saves_preserve_each_writers_dirty_field() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(&path, "[future]\nflag = \"keep\"\n").expect("seed config");
        let barrier = Arc::new(Barrier::new(3));

        let access_path = path.clone();
        let access_barrier = Arc::clone(&barrier);
        let access_writer = std::thread::spawn(move || {
            let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == ConfigField::AccessMode)
                .expect("access mode field");
            field.value = "full_access".to_string();
            field.dirty = true;
            access_barrier.wait();
            save_config_sections(&access_path, &editor)
        });

        let model_path = path.clone();
        let model_barrier = Arc::clone(&barrier);
        let model_writer = std::thread::spawn(move || {
            let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == ConfigField::Model)
                .expect("model field");
            field.value = "concurrent-model".to_string();
            field.dirty = true;
            model_barrier.wait();
            save_config_sections(&model_path, &editor)
        });

        barrier.wait();
        access_writer
            .join()
            .expect("access writer")
            .expect("access save");
        model_writer
            .join()
            .expect("model writer")
            .expect("model save");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
        assert_eq!(saved["model"]["model"].as_str(), Some("concurrent-model"));
        assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
    }

    #[test]
    fn cross_process_global_saves_preserve_each_writers_dirty_field() {
        const CHILD_ROLE_ENV: &str = "MOYAI_CONFIG_LEASE_TEST_ROLE";
        const CONFIG_PATH_ENV: &str = "MOYAI_CONFIG_LEASE_TEST_PATH";
        const START_PATH_ENV: &str = "MOYAI_CONFIG_LEASE_TEST_START";
        const TEST_NAME: &str = "tui::config_editor::tests::cross_process_global_saves_preserve_each_writers_dirty_field";

        if let Ok(role) = std::env::var(CHILD_ROLE_ENV) {
            let path = Utf8PathBuf::from(
                std::env::var(CONFIG_PATH_ENV).expect("child config path environment"),
            );
            let start_path = Utf8PathBuf::from(
                std::env::var(START_PATH_ENV).expect("child start path environment"),
            );
            let ready_path = start_path.with_file_name(format!("ready-{role}"));
            std::fs::write(&ready_path, "ready").expect("child ready marker");
            wait_for_test_file(&start_path, Duration::from_secs(5));
            let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
            let (key, value) = match role.as_str() {
                "access" => (ConfigField::AccessMode, "full_access"),
                "model" => (ConfigField::Model, "cross-process-model"),
                other => panic!("unknown child role {other}"),
            };
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == key)
                .expect("child config field");
            field.value = value.to_string();
            field.dirty = true;
            for _ in 0..8 {
                save_config_sections(&path, &editor).expect("child config save");
            }
            return;
        }

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 config path");
        let start_path =
            Utf8PathBuf::from_path_buf(temp_dir.path().join("start")).expect("utf8 start path");
        std::fs::write(&path, "[future]\nflag = \"keep\"\n").expect("seed config");
        let executable = std::env::current_exe().expect("current test executable");
        let mut children = ["access", "model"].map(|role| {
            Command::new(&executable)
                .arg("--exact")
                .arg(TEST_NAME)
                .arg("--nocapture")
                .env(CHILD_ROLE_ENV, role)
                .env(CONFIG_PATH_ENV, path.as_str())
                .env(START_PATH_ENV, start_path.as_str())
                .spawn()
                .expect("spawn config writer child")
        });
        for role in ["access", "model"] {
            wait_for_test_file(
                &start_path.with_file_name(format!("ready-{role}")),
                Duration::from_secs(5),
            );
        }
        std::fs::write(&start_path, "start").expect("release config writers");
        for child in &mut children {
            let status = child.wait().expect("config writer child status");
            assert!(status.success(), "config writer child failed: {status}");
        }

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
        assert_eq!(
            saved["model"]["model"].as_str(),
            Some("cross-process-model")
        );
        assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
    }

    fn wait_for_test_file(path: &camino::Utf8Path, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !path.exists() {
            assert!(Instant::now() < deadline, "timed out waiting for {path}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
