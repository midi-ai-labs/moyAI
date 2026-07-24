use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;
use fs2::FileExt;

use crate::cli::RunArgs;
use crate::config::ProviderEndpoint;
use crate::config::merge::apply_patch;
use crate::config::model::{
    AccessMode, ChatCompletionsReasoningParameters, PartialDoclingConfig, PartialFileGuardConfig,
    PartialFormatConfig, PartialInspectionConfig, PartialInstructionConfig, PartialLoggingConfig,
    PartialMcpConfig, PartialModelConfig, PartialMultiAgentConfig, PartialPermissionsConfig,
    PartialResolvedConfig, PartialSessionConfig, PartialShellConfig, PartialToolOutputConfig,
    PartialWorkspaceConfig, ProviderApiMode, ReasoningEffort, ReasoningSummary, ResolvedConfig,
};
use crate::error::ConfigError;

const GLOBAL_CONFIG_PATH_ENV: &str = "MOYAI_CONFIG_PATH";
pub(crate) const MAX_CONFIG_TOML_BYTES: usize = 1024 * 1024;

pub(crate) struct GlobalConfigWriteLease {
    file: File,
}

impl Drop for GlobalConfigWriteLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) fn acquire_global_config_write_lease(
    path: &Utf8Path,
) -> Result<GlobalConfigWriteLease, ConfigError> {
    let parent = path.parent().ok_or_else(|| {
        ConfigError::Message(format!("config path `{path}` has no parent directory"))
    })?;
    fs::create_dir_all(parent)?;
    let file_name = path.file_name().unwrap_or("config.toml");
    let lock_path = path.with_file_name(format!("{file_name}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path.as_std_path())?;
    file.lock_exclusive()?;
    Ok(GlobalConfigWriteLease { file })
}

pub struct ConfigLoader;

impl ConfigLoader {
    pub fn load(
        _start_dir: &Utf8Path,
        cli: Option<&RunArgs>,
    ) -> Result<ResolvedConfig, ConfigError> {
        Self::load_with_global_path(global_config_path()?, cli)
    }

    fn load_with_global_path(
        global_config_path: Utf8PathBuf,
        cli: Option<&RunArgs>,
    ) -> Result<ResolvedConfig, ConfigError> {
        let config_source = global_config_path.clone();
        let mut resolved = ResolvedConfig::default();

        if let Some(global) = read_optional(global_config_path)? {
            resolved = apply_patch(resolved, global);
        }

        validate_env_overrides()?;
        resolved = apply_patch(resolved, env_patch());

        if let Some(run_args) = cli {
            let mut patch = PartialResolvedConfig::default();
            if let Some(base_url) = &run_args.base_url_override {
                patch.model.get_or_insert_default().base_url = Some(base_url.clone());
            }
            if let Some(model) = &run_args.model_override {
                patch.model.get_or_insert_default().model = Some(model.clone());
            }
            resolved = apply_patch(resolved, patch);
        }

        resolved
            .normalize_and_validate_provider_runtime()
            .map_err(|error| {
                ConfigError::Message(format!(
                    "invalid config loaded from `{config_source}`: {error}"
                ))
            })?;
        let endpoint = ProviderEndpoint::parse(&resolved.model.base_url)
            .map_err(|error| ConfigError::Message(error.to_string()))?;
        resolved.model.base_url = endpoint.as_str().to_string();
        resolved
            .validate_workspace_boundary_roots()
            .map_err(|error| {
                ConfigError::Message(format!(
                    "invalid config loaded from `{config_source}`: {error}"
                ))
            })?;
        Ok(resolved)
    }

    pub fn ensure_default_global_config() -> Result<Utf8PathBuf, ConfigError> {
        let path = global_config_path()?;
        write_default_global_config_if_missing(&path)?;
        Ok(path)
    }
}

pub fn global_config_path() -> Result<Utf8PathBuf, ConfigError> {
    if let Ok(value) = env::var(GLOBAL_CONFIG_PATH_ENV) {
        return Ok(Utf8PathBuf::from(value));
    }
    let dirs = ProjectDirs::from("net", "midi-ai-labs", "moyai")
        .ok_or_else(|| ConfigError::Message("failed to resolve config directory".to_string()))?;
    let config_dir = Utf8PathBuf::from_path_buf(dirs.config_dir().to_path_buf())
        .map_err(|_| ConfigError::Message("config directory is not valid UTF-8".to_string()))?;
    Ok(config_dir.join("config.toml"))
}

fn read_optional(path: Utf8PathBuf) -> Result<Option<PartialResolvedConfig>, ConfigError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = read_toml_utf8_bounded(&path)?;
    let parsed = toml::from_str::<PartialResolvedConfig>(&text).map_err(|source| {
        ConfigError::ParseFile {
            path: path.to_string(),
            source,
        }
    })?;
    Ok(Some(parsed))
}

pub(crate) fn read_toml_utf8_bounded(path: &Utf8Path) -> Result<String, ConfigError> {
    let file = File::open(path.as_std_path())?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_CONFIG_TOML_BYTES as u64 {
        return Err(ConfigError::Message(format!(
            "config file `{path}` exceeds the {} byte limit",
            MAX_CONFIG_TOML_BYTES
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CONFIG_TOML_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_TOML_BYTES {
        return Err(ConfigError::Message(format!(
            "config file `{path}` exceeded the {} byte limit while it was read",
            MAX_CONFIG_TOML_BYTES
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| ConfigError::Message(format!("config file `{path}` is not valid UTF-8")))
}

fn write_default_global_config_if_missing(path: &Utf8Path) -> Result<(), ConfigError> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let defaults = default_config_patch(&ResolvedConfig::default());
    let encoded = toml::to_string_pretty(&defaults)?;
    let parent = path.parent().ok_or_else(|| {
        ConfigError::Message(format!("config path `{path}` has no parent directory"))
    })?;
    let mut temp = tempfile::NamedTempFile::new_in(parent.as_std_path())?;
    temp.write_all(encoded.as_bytes())?;
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(path.as_std_path()) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(ConfigError::Io(error.error)),
    }
}

fn default_config_patch(config: &ResolvedConfig) -> PartialResolvedConfig {
    PartialResolvedConfig {
        model: Some(PartialModelConfig {
            base_url: Some(config.model.base_url.clone()),
            model: Some(config.model.model.clone()),
            provider_metadata_mode: Some(config.model.provider_metadata_mode),
            provider_api_mode: Some(config.model.provider_api_mode),
            chat_completions_reasoning_parameters: config
                .model
                .chat_completions_reasoning_parameters,
            reasoning_effort: config.model.reasoning_effort.clone(),
            reasoning_summary: Some(config.model.reasoning_summary),
            api_key_env: config.model.api_key_env.clone().map(Some),
            extra_headers: Some(config.model.extra_headers.clone()),
            request_timeout_ms: Some(config.model.request_timeout_ms),
            stream_idle_timeout_ms: Some(config.model.stream_idle_timeout_ms),
            connect_timeout_ms: Some(config.model.connect_timeout_ms),
            max_retries: Some(config.model.max_retries),
            context_window: Some(config.model.context_window),
            max_output_tokens: Some(config.model.max_output_tokens),
            temperature: config.model.temperature,
            top_p: config.model.top_p,
            top_k: config.model.top_k,
            presence_penalty: config.model.presence_penalty,
            frequency_penalty: config.model.frequency_penalty,
            seed: config.model.seed,
            stop_sequences: Some(config.model.stop_sequences.clone()),
            supports_tools: Some(config.model.supports_tools),
            supports_reasoning: Some(config.model.supports_reasoning),
            supports_images: Some(config.model.supports_images),
            parallel_tool_calls: Some(config.model.parallel_tool_calls),
            max_parallel_predictions: Some(config.model.max_parallel_predictions),
            extra_body_json: config.model.extra_body_json.clone(),
        }),
        session: Some(PartialSessionConfig {
            overflow_margin_tokens: Some(config.session.overflow_margin_tokens),
        }),
        multi_agent: Some(PartialMultiAgentConfig {
            enabled: Some(config.multi_agent.enabled),
            mode: Some(config.multi_agent.mode),
            max_concurrent_agents: Some(config.multi_agent.max_concurrent_agents),
            max_concurrent_model_requests: Some(config.multi_agent.max_concurrent_model_requests),
        }),
        permissions: Some(PartialPermissionsConfig {
            access_mode: Some(config.permissions.access_mode),
            additional_read_roots: Some(config.permissions.additional_read_roots.clone()),
            additional_write_roots: Some(config.permissions.additional_write_roots.clone()),
        }),
        shell: Some(PartialShellConfig {
            program: config.shell.program.clone().map(Some),
            family: config.shell.family.map(Some),
            default_timeout_ms: Some(config.shell.default_timeout_ms),
            max_timeout_ms: Some(config.shell.max_timeout_ms),
            env_allowlist: Some(config.shell.env_allowlist.clone()),
            hide_windows: Some(config.shell.hide_windows),
        }),
        format: Some(PartialFormatConfig {
            default_newline: Some(config.format.default_newline),
            ensure_trailing_newline: Some(config.format.ensure_trailing_newline),
            commands: Some(config.format.commands.clone()),
        }),
        instructions: Some(PartialInstructionConfig {
            additional_files: Some(config.instructions.additional_files.clone()),
        }),
        workspace: Some(PartialWorkspaceConfig {
            extra_ignore_globs: Some(config.workspace.extra_ignore_globs.clone()),
            protected_paths: Some(config.workspace.protected_paths.clone()),
        }),
        inspection: Some(PartialInspectionConfig {
            default_max_depth: Some(config.inspection.default_max_depth),
            default_max_entries_per_dir: Some(config.inspection.default_max_entries_per_dir),
            max_extensions_reported: Some(config.inspection.max_extensions_reported),
            include_hidden_by_default: Some(config.inspection.include_hidden_by_default),
        }),
        file_guard: Some(PartialFileGuardConfig {
            max_inline_read_bytes: Some(config.file_guard.max_inline_read_bytes),
            large_file_warning_bytes: Some(config.file_guard.large_file_warning_bytes),
            blocked_read_extensions: Some(config.file_guard.blocked_read_extensions.clone()),
            structured_document_extensions: Some(
                config.file_guard.structured_document_extensions.clone(),
            ),
        }),
        docling: Some(PartialDoclingConfig {
            enabled: Some(config.docling.enabled),
            base_url: Some(config.docling.base_url.clone()),
            timeout_ms: Some(config.docling.timeout_ms),
            api_key_env: config.docling.api_key_env.clone().map(Some),
            headers: Some(config.docling.headers.clone()),
        }),
        mcp: Some(PartialMcpConfig {
            enabled: Some(config.mcp.enabled),
            servers: Some(config.mcp.servers.clone()),
        }),
        tool_output: Some(PartialToolOutputConfig {
            max_lines: Some(config.tool_output.max_lines),
            max_bytes: Some(config.tool_output.max_bytes),
            max_results: Some(config.tool_output.max_results),
        }),
        logging: Some(PartialLoggingConfig {
            verbosity: Some(config.logging.verbosity),
            json_logs: Some(config.logging.json_logs),
        }),
    }
}

fn validate_env_overrides() -> Result<(), ConfigError> {
    for name in [
        "MOYAI_MULTI_AGENT_ENABLED",
        "MOYAI_SHELL_HIDE_WINDOWS",
        "MOYAI_SUPPORTS_TOOLS",
        "MOYAI_SUPPORTS_REASONING",
        "MOYAI_SUPPORTS_IMAGES",
        "MOYAI_PARALLEL_TOOL_CALLS",
        "MOYAI_INSPECTION_INCLUDE_HIDDEN",
        "MOYAI_DOCLING_ENABLED",
        "MOYAI_MCP_ENABLED",
    ] {
        validate_parsed_env::<bool>(name)?;
    }
    for name in [
        "MOYAI_MULTI_AGENT_MAX_AGENTS",
        "MOYAI_MULTI_AGENT_MAX_MODEL_REQUESTS",
        "MOYAI_INSPECTION_MAX_DEPTH",
        "MOYAI_INSPECTION_MAX_ENTRIES_PER_DIR",
        "MOYAI_INSPECTION_MAX_EXTENSIONS_REPORTED",
        "MOYAI_OVERFLOW_MARGIN_TOKENS",
    ] {
        validate_parsed_env::<usize>(name)?;
    }
    for name in [
        "MOYAI_REQUEST_TIMEOUT_MS",
        "MOYAI_STREAM_IDLE_TIMEOUT_MS",
        "MOYAI_CONNECT_TIMEOUT_MS",
        "MOYAI_SEED",
        "MOYAI_MAX_INLINE_READ_BYTES",
        "MOYAI_LARGE_FILE_WARNING_BYTES",
        "MOYAI_DOCLING_TIMEOUT_MS",
    ] {
        validate_parsed_env::<u64>(name)?;
    }
    validate_parsed_env::<u8>("MOYAI_MAX_RETRIES")?;
    for name in [
        "MOYAI_CONTEXT_WINDOW",
        "MOYAI_MAX_OUTPUT_TOKENS",
        "MOYAI_TOP_K",
        "MOYAI_MAX_PARALLEL_PREDICTIONS",
    ] {
        validate_parsed_env::<u32>(name)?;
    }
    for name in [
        "MOYAI_TEMPERATURE",
        "MOYAI_TOP_P",
        "MOYAI_PRESENCE_PENALTY",
        "MOYAI_FREQUENCY_PENALTY",
    ] {
        if let Some(value) = env_utf8(name)? {
            validate_provider_float_env_value(name, &value)?;
        }
    }

    validate_with("MOYAI_ACCESS_MODE", |value| {
        parse_access_mode(value).is_some()
    })?;
    validate_with("MOYAI_MULTI_AGENT_MODE", |value| {
        crate::config::MultiAgentMode::parse(value).is_some()
    })?;
    validate_with("MOYAI_PROVIDER_METADATA_MODE", |value| {
        parse_provider_metadata_mode(value).is_some()
    })?;
    validate_with("MOYAI_PROVIDER_API_MODE", |value| {
        parse_provider_api_mode(value).is_some()
    })?;
    validate_with("MOYAI_CHAT_COMPLETIONS_REASONING_PARAMETERS", |value| {
        parse_chat_completions_reasoning_parameters(value).is_some()
    })?;
    validate_with("MOYAI_REASONING_EFFORT", |value| {
        parse_reasoning_effort(value).is_some()
    })?;
    validate_with("MOYAI_REASONING_SUMMARY", |value| {
        parse_reasoning_summary(value).is_some()
    })?;
    for name in ["MOYAI_EXTRA_HEADERS", "MOYAI_DOCLING_HEADERS"] {
        validate_with(name, |value| parse_string_map_json(value).is_some())?;
    }
    validate_with("MOYAI_EXTRA_BODY_JSON", |value| {
        serde_json::from_str::<serde_json::Value>(value).is_ok()
    })?;
    validate_with("MOYAI_MCP_SERVERS_JSON", |value| {
        serde_json::from_str::<Vec<crate::config::McpServerConfig>>(value).is_ok()
    })?;
    validate_with("MOYAI_MODEL", |value| !value.trim().is_empty())?;
    for name in ["MOYAI_API_KEY_ENV", "MOYAI_DOCLING_API_KEY_ENV"] {
        validate_with(name, |value| {
            let value = value.trim();
            !value.is_empty()
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })?;
    }
    // Free-form overrides still reject non-Unicode values rather than silently
    // behaving as if the variable were absent.
    for name in [
        "MOYAI_BASE_URL",
        "MOYAI_STOP_SEQUENCES",
        "MOYAI_BLOCKED_READ_EXTENSIONS",
        "MOYAI_STRUCTURED_DOCUMENT_EXTENSIONS",
        "MOYAI_DOCLING_BASE_URL",
    ] {
        let _ = env_utf8(name)?;
    }
    Ok(())
}

fn validate_parsed_env<T>(name: &str) -> Result<(), ConfigError>
where
    T: std::str::FromStr,
{
    if let Some(value) = env_utf8(name)? {
        value.parse::<T>().map_err(|_| invalid_env(name))?;
    }
    Ok(())
}

fn validate_provider_float_env_value(name: &str, value: &str) -> Result<(), ConfigError> {
    let parsed = value.parse::<f64>().map_err(|_| invalid_env(name))?;
    crate::config::model::validate_optional_provider_float(name, Some(parsed))
        .map_err(|_| invalid_env(name))
}

fn validate_with(name: &str, validate: impl FnOnce(&str) -> bool) -> Result<(), ConfigError> {
    if let Some(value) = env_utf8(name)?
        && !validate(&value)
    {
        return Err(invalid_env(name));
    }
    Ok(())
}

fn env_utf8(name: &str) -> Result<Option<String>, ConfigError> {
    std::env::var_os(name)
        .map(|value| {
            value.into_string().map_err(|_| {
                ConfigError::Message(format!(
                    "environment override `{name}` is not valid Unicode"
                ))
            })
        })
        .transpose()
}

fn invalid_env(name: &str) -> ConfigError {
    ConfigError::Message(format!(
        "environment override `{name}` has an invalid value"
    ))
}

fn env_patch() -> PartialResolvedConfig {
    let mut patch = PartialResolvedConfig::default();

    if let Ok(value) = env::var("MOYAI_BASE_URL") {
        patch.model.get_or_insert_default().base_url = Some(value);
    }
    if let Ok(value) = env::var("MOYAI_MODEL") {
        patch.model.get_or_insert_default().model = Some(value);
    }
    if let Ok(value) = env::var("MOYAI_ACCESS_MODE") {
        if let Some(parsed) = parse_access_mode(&value) {
            patch.permissions.get_or_insert_default().access_mode = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MULTI_AGENT_ENABLED")
        && let Ok(parsed) = value.parse()
    {
        patch.multi_agent.get_or_insert_default().enabled = Some(parsed);
    }
    if let Ok(value) = env::var("MOYAI_MULTI_AGENT_MODE")
        && let Some(parsed) = crate::config::MultiAgentMode::parse(&value)
    {
        patch.multi_agent.get_or_insert_default().mode = Some(parsed);
    }
    if let Ok(value) = env::var("MOYAI_MULTI_AGENT_MAX_AGENTS")
        && let Ok(parsed) = value.parse::<usize>()
    {
        patch
            .multi_agent
            .get_or_insert_default()
            .max_concurrent_agents = Some(parsed.max(1));
    }
    if let Ok(value) = env::var("MOYAI_MULTI_AGENT_MAX_MODEL_REQUESTS")
        && let Ok(parsed) = value.parse::<usize>()
    {
        patch
            .multi_agent
            .get_or_insert_default()
            .max_concurrent_model_requests = Some(parsed.max(1));
    }
    if let Ok(value) = env::var("MOYAI_SHELL_HIDE_WINDOWS") {
        if let Ok(parsed) = value.parse() {
            patch.shell.get_or_insert_default().hide_windows = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_PROVIDER_METADATA_MODE") {
        if let Some(parsed) = parse_provider_metadata_mode(&value) {
            patch.model.get_or_insert_default().provider_metadata_mode = Some(parsed);
        }
    }
    apply_reasoning_env_overrides(
        &mut patch,
        env::var("MOYAI_PROVIDER_API_MODE").ok(),
        env::var("MOYAI_CHAT_COMPLETIONS_REASONING_PARAMETERS").ok(),
        env::var("MOYAI_REASONING_EFFORT").ok(),
        env::var("MOYAI_REASONING_SUMMARY").ok(),
    );
    if let Ok(value) = env::var("MOYAI_API_KEY_ENV") {
        patch.model.get_or_insert_default().api_key_env = Some(Some(value));
    }
    if let Ok(value) = env::var("MOYAI_EXTRA_HEADERS") {
        if let Some(parsed) = parse_string_map_json(&value) {
            patch.model.get_or_insert_default().extra_headers = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_REQUEST_TIMEOUT_MS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().request_timeout_ms = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_STREAM_IDLE_TIMEOUT_MS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().stream_idle_timeout_ms = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_CONNECT_TIMEOUT_MS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().connect_timeout_ms = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MAX_RETRIES") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().max_retries = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_CONTEXT_WINDOW") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().context_window = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MAX_OUTPUT_TOKENS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().max_output_tokens = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_TEMPERATURE") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().temperature = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_TOP_P") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().top_p = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_TOP_K") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().top_k = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_PRESENCE_PENALTY") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().presence_penalty = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_FREQUENCY_PENALTY") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().frequency_penalty = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_SEED") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().seed = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_STOP_SEQUENCES") {
        let parsed = value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        patch.model.get_or_insert_default().stop_sequences = Some(parsed);
    }
    if let Ok(value) = env::var("MOYAI_SUPPORTS_TOOLS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().supports_tools = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_SUPPORTS_REASONING") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().supports_reasoning = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_SUPPORTS_IMAGES") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().supports_images = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_PARALLEL_TOOL_CALLS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().parallel_tool_calls = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MAX_PARALLEL_PREDICTIONS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().max_parallel_predictions = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_EXTRA_BODY_JSON") {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&value) {
            patch.model.get_or_insert_default().extra_body_json = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_INSPECTION_MAX_DEPTH") {
        if let Ok(parsed) = value.parse() {
            patch.inspection.get_or_insert_default().default_max_depth = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_INSPECTION_MAX_ENTRIES_PER_DIR") {
        if let Ok(parsed) = value.parse() {
            patch
                .inspection
                .get_or_insert_default()
                .default_max_entries_per_dir = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_INSPECTION_MAX_EXTENSIONS_REPORTED") {
        if let Ok(parsed) = value.parse() {
            patch
                .inspection
                .get_or_insert_default()
                .max_extensions_reported = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_INSPECTION_INCLUDE_HIDDEN") {
        if let Ok(parsed) = value.parse() {
            patch
                .inspection
                .get_or_insert_default()
                .include_hidden_by_default = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MAX_INLINE_READ_BYTES") {
        if let Ok(parsed) = value.parse() {
            patch
                .file_guard
                .get_or_insert_default()
                .max_inline_read_bytes = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_LARGE_FILE_WARNING_BYTES") {
        if let Ok(parsed) = value.parse() {
            patch
                .file_guard
                .get_or_insert_default()
                .large_file_warning_bytes = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_BLOCKED_READ_EXTENSIONS") {
        let parsed = value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
            .collect::<Vec<_>>();
        patch
            .file_guard
            .get_or_insert_default()
            .blocked_read_extensions = Some(parsed);
    }
    if let Ok(value) = env::var("MOYAI_STRUCTURED_DOCUMENT_EXTENSIONS") {
        let parsed = value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
            .collect::<Vec<_>>();
        patch
            .file_guard
            .get_or_insert_default()
            .structured_document_extensions = Some(parsed);
    }
    if let Ok(value) = env::var("MOYAI_DOCLING_ENABLED") {
        if let Ok(parsed) = value.parse() {
            patch.docling.get_or_insert_default().enabled = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_DOCLING_BASE_URL") {
        patch.docling.get_or_insert_default().base_url = Some(value);
    }
    if let Ok(value) = env::var("MOYAI_DOCLING_TIMEOUT_MS") {
        if let Ok(parsed) = value.parse() {
            patch.docling.get_or_insert_default().timeout_ms = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_DOCLING_API_KEY_ENV") {
        patch.docling.get_or_insert_default().api_key_env = Some(Some(value));
    }
    if let Ok(value) = env::var("MOYAI_DOCLING_HEADERS") {
        if let Some(parsed) = parse_string_map_json(&value) {
            patch.docling.get_or_insert_default().headers = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MCP_ENABLED") {
        if let Ok(parsed) = value.parse() {
            patch.mcp.get_or_insert_default().enabled = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MCP_SERVERS_JSON") {
        if let Ok(parsed) = serde_json::from_str::<Vec<crate::config::McpServerConfig>>(&value) {
            patch.mcp.get_or_insert_default().servers = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_OVERFLOW_MARGIN_TOKENS") {
        if let Ok(parsed) = value.parse() {
            patch.session.get_or_insert_default().overflow_margin_tokens = Some(parsed);
        }
    }
    patch
}

fn apply_reasoning_env_overrides(
    patch: &mut PartialResolvedConfig,
    provider_api_mode: Option<String>,
    chat_completions_reasoning_parameters: Option<String>,
    reasoning_effort: Option<String>,
    reasoning_summary: Option<String>,
) {
    if let Some(value) = provider_api_mode
        && let Some(parsed) = parse_provider_api_mode(&value)
    {
        patch.model.get_or_insert_default().provider_api_mode = Some(parsed);
    }
    if let Some(value) = chat_completions_reasoning_parameters
        && let Some(parsed) = parse_chat_completions_reasoning_parameters(&value)
    {
        patch
            .model
            .get_or_insert_default()
            .chat_completions_reasoning_parameters = Some(parsed);
    }
    if let Some(value) = reasoning_effort
        && let Some(parsed) = parse_reasoning_effort(&value)
    {
        patch.model.get_or_insert_default().reasoning_effort = Some(parsed);
    }
    if let Some(value) = reasoning_summary
        && let Some(parsed) = parse_reasoning_summary(&value)
    {
        patch.model.get_or_insert_default().reasoning_summary = Some(parsed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_global_config_is_created_with_editable_defaults() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 path");

        write_default_global_config_if_missing(&path).expect("write default config");

        let text = fs::read_to_string(path).expect("read config");
        assert!(text.contains("[model]"));
        assert!(text.contains("base_url = \"http://127.0.0.1:1234\""));
        assert!(text.contains("model = \"qwen/qwen3.6-35b-a3b\""));
        assert!(text.contains("provider_metadata_mode = \"lm_studio_native_required\""));
        assert!(text.contains("provider_api_mode = \"responses\""));
        assert!(text.contains("reasoning_summary = \"none\""));
        assert!(!text.contains("chat_completions_reasoning_parameters"));
        assert!(!text.contains("reasoning_effort"));
        assert!(text.contains("request_timeout_ms = 600000"));
        assert!(text.contains("stream_idle_timeout_ms = 600000"));
        assert!(text.contains("max_output_tokens = 16384"));
        assert!(!text.contains("prompt_profile"));
        assert!(!text.contains("max_steps_per_turn"));
        assert!(text.contains("[docling]"));
        assert!(text.contains("enabled = false"));
        assert!(text.contains("base_url = \"http://127.0.0.1:8123\""));
        assert!(text.contains("base_url = \"http://127.0.0.1:8123/mcp\""));
        assert!(text.contains("[permissions]"));
        assert!(!text.contains("[agent]"));
        let generated =
            toml::from_str::<PartialResolvedConfig>(&text).expect("generated config parses");
        assert_eq!(
            generated
                .multi_agent
                .and_then(|multi_agent| multi_agent.enabled),
            Some(true)
        );
    }

    #[test]
    fn packaged_config_example_uses_canonical_current_defaults() {
        let text = include_str!("../../config.example.toml");
        let document = toml::from_str::<toml::Value>(text).expect("config example TOML");

        assert_eq!(
            document["permissions"]["access_mode"].as_str(),
            Some("default")
        );
        assert_eq!(document["multi_agent"]["enabled"].as_bool(), Some(true));
        assert_eq!(
            document["model"]["provider_api_mode"].as_str(),
            Some("responses")
        );

        let patch = toml::from_str::<PartialResolvedConfig>(text)
            .expect("config example follows the strict current schema");
        let resolved = apply_patch(ResolvedConfig::default(), patch);
        assert_eq!(
            serde_json::to_value(resolved).expect("resolved example"),
            serde_json::to_value(ResolvedConfig::default()).expect("current defaults")
        );
    }

    #[test]
    fn default_global_config_does_not_overwrite_existing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 path");
        fs::write(&path, "[model]\nmodel = \"custom\"\n").expect("seed config");

        write_default_global_config_if_missing(&path).expect("preserve config");

        let text = fs::read_to_string(path).expect("read config");
        assert_eq!(text, "[model]\nmodel = \"custom\"\n");
    }

    #[test]
    fn load_uses_global_config_and_ignores_workspace_config_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 path");
        fs::create_dir_all(root.join(".moyai")).expect("workspace dirs");
        fs::write(
            root.join("moyai.toml"),
            "[model]\nmodel = \"workspace-primary\"\nbase_url = \"http://workspace-primary\"\n",
        )
        .expect("workspace primary config");
        fs::write(
            root.join(".moyai").join("config.toml"),
            "[model]\nmodel = \"workspace-secondary\"\nbase_url = \"http://workspace-secondary\"\n",
        )
        .expect("workspace secondary config");

        let global = Utf8PathBuf::from_path_buf(temp.path().join("global.toml")).expect("utf8");
        fs::write(
            &global,
            "[model]\nmodel = \"global-model\"\nbase_url = \"http://global\"\n",
        )
        .expect("global config");

        let config = ConfigLoader::load_with_global_path(global, None).expect("load config");

        assert_eq!(config.model.model, "global-model");
        assert_eq!(config.model.base_url, "http://global");
    }

    #[test]
    fn removed_agent_section_is_rejected_instead_of_becoming_a_noop_contract() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 path");
        fs::write(
            &path,
            "[model]\nmodel = \"current-model\"\n\n[agent]\nduplicate_success_abort_threshold = 99\nstaged_task_recovery_stall_threshold = 77\n",
        )
        .expect("legacy config");

        let error = ConfigLoader::load_with_global_path(path, None)
            .expect_err("removed config section must fail closed");

        assert!(error.to_string().contains("agent"));
    }

    #[test]
    fn removed_stream_retry_setting_is_rejected_instead_of_silently_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 path");
        fs::write(
            &path,
            "[model]\nmodel = \"current-model\"\nstream_max_retries = 2\n",
        )
        .expect("obsolete config");

        let expected_path = path.to_string();
        let error = ConfigLoader::load_with_global_path(path, None)
            .expect_err("removed transport contract must fail closed");

        assert!(error.to_string().contains("stream_max_retries"));
        assert!(error.to_string().contains(&expected_path));
    }

    #[test]
    fn relative_workspace_boundary_roots_are_rejected_with_the_exact_field() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (section, field) in [
            ("permissions", "additional_read_roots"),
            ("permissions", "additional_write_roots"),
            ("workspace", "protected_paths"),
        ] {
            let path =
                Utf8PathBuf::from_path_buf(temp.path().join(format!("{section}-{field}.toml")))
                    .expect("utf8 path");
            fs::write(
                &path,
                format!("[{section}]\n{field} = [\"relative/safety-root\"]\n"),
            )
            .expect("relative root config");

            let error = ConfigLoader::load_with_global_path(path.clone(), None)
                .expect_err("relative boundary roots must fail closed");
            let diagnostic = error.to_string();
            assert!(diagnostic.contains(&format!("{section}.{field}")));
            assert!(diagnostic.contains("absolute path"));
            assert!(diagnostic.contains(path.as_str()));
        }
    }

    #[test]
    fn provider_runtime_fields_are_canonicalized_and_bounded_at_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        let canonical_path =
            Utf8PathBuf::from_path_buf(temp.path().join("canonical-model.toml")).expect("utf8");
        fs::write(
            &canonical_path,
            "[model]\nmodel = \"  canonical-model  \"\n",
        )
        .expect("canonical model config");
        let canonical = ConfigLoader::load_with_global_path(canonical_path, None)
            .expect("canonical provider runtime config");
        assert_eq!(canonical.model.model, "canonical-model");

        for (name, body, field) in [
            ("blank-model", "[model]\nmodel = \" \\t \"\n", "model.model"),
            (
                "zero-response-start-timeout",
                "[model]\nrequest_timeout_ms = 0\n",
                "model.request_timeout_ms",
            ),
            (
                "nan-temperature",
                "[model]\ntemperature = nan\n",
                "model.temperature",
            ),
            ("infinite-top-p", "[model]\ntop_p = inf\n", "model.top_p"),
            (
                "negative-infinite-presence-penalty",
                "[model]\npresence_penalty = -inf\n",
                "model.presence_penalty",
            ),
            (
                "nan-frequency-penalty",
                "[model]\nfrequency_penalty = nan\n",
                "model.frequency_penalty",
            ),
        ] {
            let path = Utf8PathBuf::from_path_buf(temp.path().join(format!("{name}.toml")))
                .expect("utf8 path");
            fs::write(&path, body).expect("invalid provider config");
            let error = ConfigLoader::load_with_global_path(path.clone(), None)
                .expect_err("invalid provider runtime config must fail closed");
            let diagnostic = error.to_string();
            assert!(diagnostic.contains(field));
            assert!(diagnostic.contains(path.as_str()));
        }
    }

    #[test]
    fn provider_float_environment_values_share_the_finite_runtime_contract() {
        for name in [
            "MOYAI_TEMPERATURE",
            "MOYAI_TOP_P",
            "MOYAI_PRESENCE_PENALTY",
            "MOYAI_FREQUENCY_PENALTY",
        ] {
            assert!(validate_provider_float_env_value(name, "0.5").is_ok());
            for non_finite in ["NaN", "inf", "-inf"] {
                let error = validate_provider_float_env_value(name, non_finite)
                    .expect_err("non-finite environment value must fail closed");
                assert!(error.to_string().contains(name));
            }
        }
    }

    #[test]
    fn provider_endpoint_is_canonicalized_and_url_borne_secrets_are_rejected_at_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root_path = Utf8PathBuf::from_path_buf(temp.path().join("root.toml")).expect("utf8");
        fs::write(
            &root_path,
            "[model]\nbase_url = \"http://m4macmini.local:1234/\"\n",
        )
        .expect("root config");
        let root = ConfigLoader::load_with_global_path(root_path, None).expect("root URL");
        assert_eq!(root.model.base_url, "http://m4macmini.local:1234");

        let v1_path = Utf8PathBuf::from_path_buf(temp.path().join("v1.toml")).expect("utf8");
        fs::write(
            &v1_path,
            "[model]\nbase_url = \"http://m4macmini.local:1234/v1/\"\n",
        )
        .expect("v1 config");
        let v1 = ConfigLoader::load_with_global_path(v1_path, None).expect("v1 URL");
        assert_eq!(v1.model.base_url, "http://m4macmini.local:1234/v1");

        for (name, endpoint) in [
            ("userinfo", "https://user:secret@provider.example/v1"),
            ("query", "https://provider.example/v1?api_key=hidden"),
            ("fragment", "https://provider.example/v1#debug"),
        ] {
            let path =
                Utf8PathBuf::from_path_buf(temp.path().join(format!("{name}.toml"))).expect("utf8");
            fs::write(&path, format!("[model]\nbase_url = \"{endpoint}\"\n"))
                .expect("invalid config");
            let error = ConfigLoader::load_with_global_path(path, None)
                .expect_err("URL-borne secret must be rejected");
            let diagnostic = format!("{error:?}: {error}");
            assert!(!diagnostic.contains("secret"));
            assert!(!diagnostic.contains("hidden"));
            assert!(!diagnostic.contains(endpoint));
        }
    }

    #[test]
    fn concurrent_default_config_creation_is_noclobbering() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 path");
        let handles = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || write_default_global_config_if_missing(&path))
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().expect("writer thread").expect("write config");
        }

        let text = fs::read_to_string(&path).expect("read config");
        toml::from_str::<PartialResolvedConfig>(&text).expect("complete config");
    }

    #[test]
    fn config_reader_rejects_oversized_and_non_utf8_input() {
        let temp = tempfile::tempdir().expect("tempdir");
        let oversized =
            Utf8PathBuf::from_path_buf(temp.path().join("oversized.toml")).expect("utf8 path");
        let file = File::create(&oversized).expect("oversized fixture");
        file.set_len(MAX_CONFIG_TOML_BYTES as u64 + 1)
            .expect("sparse length");
        assert!(
            read_toml_utf8_bounded(&oversized)
                .expect_err("oversized config must fail")
                .to_string()
                .contains("byte limit")
        );

        let invalid =
            Utf8PathBuf::from_path_buf(temp.path().join("invalid.toml")).expect("utf8 path");
        fs::write(&invalid, [0xff, 0xfe]).expect("invalid UTF-8 fixture");
        assert!(
            read_toml_utf8_bounded(&invalid)
                .expect_err("non UTF-8 config must fail")
                .to_string()
                .contains("UTF-8")
        );
    }

    #[test]
    fn reasoning_environment_overrides_are_typed_and_keep_model_capability_independent() {
        let mut patch = PartialResolvedConfig::default();
        apply_reasoning_env_overrides(
            &mut patch,
            Some("chat-completions".to_string()),
            Some("effort-and-summary".to_string()),
            Some("HIGH".to_string()),
            Some("concise".to_string()),
        );

        let resolved = apply_patch(ResolvedConfig::default(), patch);
        assert_eq!(
            resolved.model.provider_api_mode,
            ProviderApiMode::ChatCompletions
        );
        assert_eq!(
            resolved.model.chat_completions_reasoning_parameters,
            Some(ChatCompletionsReasoningParameters::EffortAndSummary)
        );
        assert_eq!(resolved.model.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(resolved.model.reasoning_summary, ReasoningSummary::Concise);
        assert!(!resolved.model.supports_reasoning);
    }

    #[test]
    fn reasoning_environment_value_parsers_cover_supported_contracts() {
        assert_eq!(
            parse_provider_api_mode("auto"),
            Some(ProviderApiMode::Responses)
        );
        assert_eq!(
            parse_provider_api_mode("responses"),
            Some(ProviderApiMode::Responses)
        );
        assert_eq!(
            parse_chat_completions_reasoning_parameters("effort_only"),
            Some(ChatCompletionsReasoningParameters::EffortOnly)
        );
        assert_eq!(
            parse_chat_completions_reasoning_parameters("effort-and-summary"),
            Some(ChatCompletionsReasoningParameters::EffortAndSummary)
        );
        assert_eq!(
            parse_reasoning_effort("medium"),
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(
            parse_reasoning_effort("provider_future_effort"),
            Some(ReasoningEffort::Custom(
                "provider_future_effort".to_string()
            ))
        );
        assert_eq!(
            parse_reasoning_summary("detailed"),
            Some(ReasoningSummary::Detailed)
        );
        assert_eq!(parse_provider_api_mode("invalid"), None);
        assert_eq!(parse_chat_completions_reasoning_parameters("invalid"), None);
        assert_eq!(parse_reasoning_effort("  "), None);
        assert_eq!(parse_reasoning_summary("invalid"), None);
    }
}

fn parse_provider_metadata_mode(value: &str) -> Option<crate::config::model::ProviderMetadataMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "lm_studio_native_required"
        | "lm-studio-native-required"
        | "lmstudio"
        | "lm_studio"
        | "lm-studio" => Some(crate::config::model::ProviderMetadataMode::LmStudioNativeRequired),
        "openai_compatible_only"
        | "openai-compatible-only"
        | "openai"
        | "openai_compat"
        | "openai-compatible" => {
            Some(crate::config::model::ProviderMetadataMode::OpenAiCompatibleOnly)
        }
        _ => None,
    }
}

fn parse_provider_api_mode(value: &str) -> Option<ProviderApiMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        // One-way compatibility normalization. Runtime state has no Auto mode;
        // legacy config deterministically becomes Responses.
        "auto" => Some(ProviderApiMode::Responses),
        "chat_completions" | "chat-completions" | "chat" => Some(ProviderApiMode::ChatCompletions),
        "responses" | "response" => Some(ProviderApiMode::Responses),
        _ => None,
    }
}

fn parse_chat_completions_reasoning_parameters(
    value: &str,
) -> Option<ChatCompletionsReasoningParameters> {
    match value.trim().to_ascii_lowercase().as_str() {
        "effort_only" | "effort-only" | "effort" => {
            Some(ChatCompletionsReasoningParameters::EffortOnly)
        }
        "effort_and_summary" | "effort-and-summary" | "summary" => {
            Some(ChatCompletionsReasoningParameters::EffortAndSummary)
        }
        _ => None,
    }
}

fn parse_reasoning_effort(value: &str) -> Option<ReasoningEffort> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_ascii_lowercase();
    match normalized.as_str() {
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra" => {
            normalized.parse().ok()
        }
        "x_high" | "x-high" => Some(ReasoningEffort::XHigh),
        _ => trimmed.parse().ok(),
    }
}

fn parse_reasoning_summary(value: &str) -> Option<ReasoningSummary> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(ReasoningSummary::None),
        "auto" => Some(ReasoningSummary::Auto),
        "concise" => Some(ReasoningSummary::Concise),
        "detailed" => Some(ReasoningSummary::Detailed),
        _ => None,
    }
}

fn parse_access_mode(value: &str) -> Option<AccessMode> {
    AccessMode::parse(value)
}

fn parse_string_map_json(value: &str) -> Option<std::collections::BTreeMap<String, String>> {
    serde_json::from_str(value).ok()
}
