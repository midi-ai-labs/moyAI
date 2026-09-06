use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;
use fs2::FileExt;

use crate::cli::RunArgs;
use crate::config::ProviderEndpoint;
use crate::config::merge::{
    apply_patch, normalize_provider_profile_alias, normalize_request_timeout_alias,
};
use crate::config::model::{
    AccessMode, PartialDoclingConfig, PartialFileGuardConfig, PartialFormatConfig,
    PartialInspectionConfig, PartialInstructionConfig, PartialLoggingConfig, PartialMcpConfig,
    PartialModelConfig, PartialMultiAgentConfig, PartialPermissionsConfig, PartialResolvedConfig,
    PartialSessionConfig, PartialShellConfig, PartialSideChatConfig, PartialToolOutputConfig,
    PartialWorkspaceConfig, ProviderApiMode, ProviderProfile, ResolvedConfig,
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

#[derive(Debug)]
pub(crate) struct ForwardCompatibleGlobalConfigResolution {
    pub resolved_config: ResolvedConfig,
    pub preserved_unknown_top_level_sections: Vec<String>,
}

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
        let global = read_optional(global_config_path)?;
        Self::resolve_global_config(&config_source, global, cli)
    }

    #[cfg(feature = "tauri-desktop")]
    pub(crate) fn validate_global_config_text(
        config_source: &Utf8Path,
        text: &str,
    ) -> Result<(), ConfigError> {
        Self::resolve_global_config_text_without_environment(config_source, text).map(drop)
    }

    pub(crate) fn resolve_global_config_text_without_environment(
        config_source: &Utf8Path,
        text: &str,
    ) -> Result<ResolvedConfig, ConfigError> {
        let global = parse_global_config_text(config_source, text)?;
        Self::resolve_config(config_source, Some(global), None, None)
    }

    pub(crate) fn resolve_global_config_text_with_environment(
        config_source: &Utf8Path,
        text: &str,
    ) -> Result<ResolvedConfig, ConfigError> {
        let global = parse_global_config_text(config_source, text)?;
        Self::resolve_global_config(config_source, Some(global), None)
    }

    pub(crate) fn resolve_forward_compatible_global_config_text_with_environment(
        config_source: &Utf8Path,
        text: &str,
    ) -> Result<ForwardCompatibleGlobalConfigResolution, ConfigError> {
        let (global, preserved_unknown_top_level_sections) =
            parse_forward_compatible_global_config_text(config_source, text)?;
        let resolved_config = Self::resolve_global_config(config_source, Some(global), None)?;
        Ok(ForwardCompatibleGlobalConfigResolution {
            resolved_config,
            preserved_unknown_top_level_sections,
        })
    }

    fn resolve_global_config(
        config_source: &Utf8Path,
        global: Option<PartialResolvedConfig>,
        cli: Option<&RunArgs>,
    ) -> Result<ResolvedConfig, ConfigError> {
        validate_env_overrides()?;
        Self::resolve_config(config_source, global, Some(env_patch()?), cli)
    }

    fn resolve_config(
        config_source: &Utf8Path,
        global: Option<PartialResolvedConfig>,
        environment: Option<PartialResolvedConfig>,
        cli: Option<&RunArgs>,
    ) -> Result<ResolvedConfig, ConfigError> {
        let mut resolved = ResolvedConfig::default();

        if let Some(mut global) = global {
            normalize_request_timeout_alias(
                &mut global,
                "model.request_timeout_ms",
                "model.stream_idle_timeout_ms",
            )
            .map_err(|error| {
                ConfigError::Message(format!(
                    "invalid config loaded from `{config_source}`: {error}"
                ))
            })?;
            normalize_provider_profile_alias(
                &mut global,
                resolved.model.provider_profile,
                "model.provider_profile",
                "model.provider_metadata_mode",
                "model.provider_api_mode",
            )
            .map_err(|error| {
                ConfigError::Message(format!(
                    "invalid config loaded from `{config_source}`: {error}"
                ))
            })?;
            resolved = apply_patch(resolved, global);
        }

        if let Some(mut environment) = environment {
            normalize_request_timeout_alias(
                &mut environment,
                "MOYAI_REQUEST_TIMEOUT_MS",
                "MOYAI_STREAM_IDLE_TIMEOUT_MS",
            )
            .map_err(ConfigError::Message)?;
            normalize_provider_profile_alias(
                &mut environment,
                resolved.model.provider_profile,
                "MOYAI_PROVIDER_PROFILE",
                "MOYAI_PROVIDER_METADATA_MODE",
                "MOYAI_PROVIDER_API_MODE",
            )
            .map_err(ConfigError::Message)?;
            resolved = apply_patch(resolved, environment);
        }

        if let Some(run_args) = cli {
            let mut patch = run_args
                .provider_connection_override
                .config_patch()
                .unwrap_or_default();
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
        resolved
            .normalize_and_validate_docling_runtime()
            .map_err(|error| {
                ConfigError::Message(format!(
                    "invalid config loaded from `{config_source}`: {error}"
                ))
            })?;
        resolved
            .normalize_and_validate_mcp_runtime()
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
    parse_global_config_text(&path, &text).map(Some)
}

fn parse_global_config_text(
    path: &Utf8Path,
    text: &str,
) -> Result<PartialResolvedConfig, ConfigError> {
    toml::from_str::<PartialResolvedConfig>(text).map_err(|source| ConfigError::ParseFile {
        path: path.to_string(),
        source,
    })
}

fn parse_forward_compatible_global_config_text(
    path: &Utf8Path,
    text: &str,
) -> Result<(PartialResolvedConfig, Vec<String>), ConfigError> {
    let mut document =
        toml::from_str::<toml::Value>(text).map_err(|source| ConfigError::ParseFile {
            path: path.to_string(),
            source,
        })?;
    let root = document.as_table_mut().ok_or_else(|| {
        ConfigError::Message(format!("config loaded from `{path}` must be a TOML table"))
    })?;
    let mut preserved_unknown_top_level_sections = root
        .keys()
        .filter(|section| {
            !PartialResolvedConfig::CURRENT_TOP_LEVEL_SECTIONS.contains(&section.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    preserved_unknown_top_level_sections.sort();
    root.retain(|section, _| PartialResolvedConfig::CURRENT_TOP_LEVEL_SECTIONS.contains(&section));
    let current_schema_text = toml::to_string(&document).map_err(|error| {
        ConfigError::Message(format!(
            "failed to project current config sections from `{path}`: {error}"
        ))
    })?;
    parse_global_config_text(path, &current_schema_text)
        .map(|config| (config, preserved_unknown_top_level_sections))
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
            system_prompt: Some(config.model.system_prompt.clone()),
            provider_profile: Some(config.model.provider_profile),
            provider_metadata_mode: None,
            provider_api_mode: None,
            chat_completions_reasoning_parameters: None,
            reasoning_effort: None,
            reasoning_summary: None,
            api_key_env: config.model.api_key_env.clone().map(Some),
            extra_headers: Some(config.model.extra_headers.clone()),
            request_timeout_ms: Some(config.model.request_timeout_ms),
            legacy_stream_idle_timeout_ms: None,
            connect_timeout_ms: Some(config.model.connect_timeout_ms),
            max_retries: Some(config.model.max_retries),
            context_window: Some(config.model.context_window),
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            stop_sequences: None,
            supports_tools: Some(config.model.supports_tools),
            supports_reasoning: None,
            supports_images: Some(config.model.supports_images),
            parallel_tool_calls: Some(config.model.parallel_tool_calls),
            max_parallel_predictions: Some(config.model.max_parallel_predictions),
            extra_body_json: None,
        }),
        side_chat: Some(PartialSideChatConfig {
            base_url: Some(config.side_chat.base_url.clone()),
            model: Some(config.side_chat.model.clone()),
            system_prompt: Some(config.side_chat.system_prompt.clone()),
            provider_profile: Some(config.side_chat.provider_profile),
            context_window: Some(config.side_chat.context_window),
            request_timeout_ms: Some(config.side_chat.request_timeout_ms),
            connect_timeout_ms: Some(config.side_chat.connect_timeout_ms),
            max_retries: Some(config.side_chat.max_retries),
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
        device_network: Some(config.device_network.clone()),
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
        "MOYAI_MAX_INLINE_READ_BYTES",
        "MOYAI_LARGE_FILE_WARNING_BYTES",
        "MOYAI_DOCLING_TIMEOUT_MS",
    ] {
        validate_parsed_env::<u64>(name)?;
    }
    validate_parsed_env::<u8>("MOYAI_MAX_RETRIES")?;
    for name in ["MOYAI_CONTEXT_WINDOW", "MOYAI_MAX_PARALLEL_PREDICTIONS"] {
        validate_parsed_env::<u32>(name)?;
    }

    validate_with("MOYAI_ACCESS_MODE", |value| {
        parse_access_mode(value).is_some()
    })?;
    validate_with("MOYAI_MULTI_AGENT_MODE", |value| {
        crate::config::MultiAgentMode::parse(value).is_some()
    })?;
    validate_with("MOYAI_PROVIDER_PROFILE", |value| {
        ProviderProfile::parse(value).is_some()
    })?;
    validate_with("MOYAI_PROVIDER_METADATA_MODE", |value| {
        parse_provider_metadata_mode(value).is_some()
    })?;
    validate_with("MOYAI_PROVIDER_API_MODE", |value| {
        parse_provider_api_mode(value).is_some()
    })?;
    for name in ["MOYAI_EXTRA_HEADERS", "MOYAI_DOCLING_HEADERS"] {
        validate_with(name, |value| parse_string_map_json(value).is_some())?;
    }
    validate_with("MOYAI_MCP_SERVERS_JSON", |value| {
        serde_json::from_str::<Vec<crate::config::McpServerConfig>>(value).is_ok()
    })?;
    validate_with("MOYAI_MODEL", |value| !value.trim().is_empty())?;
    validate_with("MOYAI_API_KEY_ENV", |value| {
        crate::config::canonical_api_key_env_name(Some(value)).is_ok()
    })?;
    validate_with("MOYAI_DOCLING_API_KEY_ENV", |value| {
        let value = value.trim();
        !value.is_empty()
            && value
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
    })?;
    // Free-form overrides still reject non-Unicode values rather than silently
    // behaving as if the variable were absent.
    for name in [
        "MOYAI_BASE_URL",
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

fn env_patch() -> Result<PartialResolvedConfig, ConfigError> {
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
    if let Ok(value) = env::var("MOYAI_PROVIDER_PROFILE")
        && let Some(parsed) = ProviderProfile::parse(&value)
    {
        patch.model.get_or_insert_default().provider_profile = Some(parsed);
    }
    if let Ok(value) = env::var("MOYAI_PROVIDER_METADATA_MODE") {
        if let Some(parsed) = parse_provider_metadata_mode(&value) {
            patch.model.get_or_insert_default().provider_metadata_mode = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_PROVIDER_API_MODE")
        && let Some(parsed) = parse_provider_api_mode(&value)
    {
        patch.model.get_or_insert_default().provider_api_mode = Some(parsed);
    }
    if let Ok(value) = env::var("MOYAI_API_KEY_ENV") {
        patch.model.get_or_insert_default().api_key_env = Some(Some(value));
    }
    if let Ok(value) = env::var("MOYAI_EXTRA_HEADERS") {
        if let Some(parsed) = parse_string_map_json(&value) {
            patch.model.get_or_insert_default().extra_headers = Some(parsed);
        }
    }
    apply_request_timeout_env_overrides(
        &mut patch,
        parse_u64_env_override("MOYAI_REQUEST_TIMEOUT_MS")?,
        parse_u64_env_override("MOYAI_STREAM_IDLE_TIMEOUT_MS")?,
    )?;
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
    if let Ok(value) = env::var("MOYAI_SUPPORTS_TOOLS") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().supports_tools = Some(parsed);
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
    Ok(patch)
}

fn parse_u64_env_override(name: &str) -> Result<Option<u64>, ConfigError> {
    env_utf8(name)?
        .map(|value| value.parse::<u64>().map_err(|_| invalid_env(name)))
        .transpose()
}

fn apply_request_timeout_env_overrides(
    patch: &mut PartialResolvedConfig,
    canonical: Option<u64>,
    legacy: Option<u64>,
) -> Result<(), ConfigError> {
    if canonical.is_none() && legacy.is_none() {
        return Ok(());
    }
    let model = patch.model.get_or_insert_default();
    model.request_timeout_ms = canonical;
    model.legacy_stream_idle_timeout_ms = legacy;
    normalize_request_timeout_alias(
        patch,
        "MOYAI_REQUEST_TIMEOUT_MS",
        "MOYAI_STREAM_IDLE_TIMEOUT_MS",
    )
    .map_err(ConfigError::Message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_import_resolution_does_not_materialize_environment_overrides() {
        let source = Utf8Path::new("import.toml");
        let text = "[model]\nmodel = \"file-model\"\n";
        if std::env::var_os("MOYAI_IMPORT_DRAFT_ENV_TEST_CHILD").is_some() {
            let resolved =
                ConfigLoader::resolve_global_config_text_without_environment(source, text)
                    .expect("strict imported draft");
            assert_eq!(resolved.model.model, "file-model");
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().expect("current test exe"))
            .args([
                "--exact",
                "config::loader::tests::draft_import_resolution_does_not_materialize_environment_overrides",
                "--nocapture",
            ])
            .env("MOYAI_IMPORT_DRAFT_ENV_TEST_CHILD", "1")
            .env("MOYAI_MODEL", "environment-model")
            .output()
            .expect("isolated import resolution test");
        assert!(
            output.status.success(),
            "isolated import resolution failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn default_global_config_is_created_with_editable_defaults() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 path");

        write_default_global_config_if_missing(&path).expect("write default config");

        let text = fs::read_to_string(path).expect("read config");
        assert!(text.contains("[model]"));
        assert!(text.contains("base_url = \"http://127.0.0.1:1234\""));
        assert!(text.contains("model = \"qwen/qwen3.6-27b\""));
        assert!(text.contains("provider_profile = \"lm_studio\""));
        assert!(!text.contains("provider_metadata_mode"));
        assert!(!text.contains("provider_api_mode"));
        for host_owned_key in [
            "chat_completions_reasoning_parameters",
            "reasoning_effort",
            "reasoning_summary",
            "max_output_tokens",
            "temperature",
            "top_p",
            "top_k",
            "presence_penalty",
            "frequency_penalty",
            "seed",
            "stop_sequences",
            "supports_reasoning",
            "extra_body_json",
        ] {
            assert!(
                !text.contains(host_owned_key),
                "new config must not advertise the legacy host-owned key {host_owned_key}"
            );
        }
        assert!(text.contains("request_timeout_ms = 3600000"));
        assert!(!text.contains("stream_idle_timeout_ms"));
        assert!(!text.contains("prompt_profile"));
        assert!(!text.contains("max_steps_per_turn"));
        assert!(text.contains("[side_chat]"));
        let document = toml::from_str::<toml::Value>(&text).expect("generated config document");
        let side_chat = document["side_chat"]
            .as_table()
            .expect("generated Side Chat defaults");
        assert_eq!(
            side_chat.get("base_url").and_then(toml::Value::as_str),
            Some("http://127.0.0.1:1234")
        );
        assert_eq!(
            side_chat
                .get("provider_profile")
                .and_then(toml::Value::as_str),
            Some("lm_studio")
        );
        for excluded in [
            "api_key_env",
            "extra_headers",
            "supports_tools",
            "supports_images",
        ] {
            assert!(!side_chat.contains_key(excluded), "unexpected {excluded}");
        }
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
            document["model"]["provider_profile"].as_str(),
            Some("lm_studio")
        );
        assert_eq!(
            document["side_chat"]["provider_profile"].as_str(),
            Some("lm_studio")
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
    fn global_config_loads_and_normalizes_the_main_system_prompt() {
        let config = ConfigLoader::resolve_global_config_text_without_environment(
            Utf8Path::new("system-prompt.toml"),
            "[model]\nsystem_prompt = \"  first\\n  second  \"\n",
        )
        .expect("valid main system prompt config");

        assert_eq!(config.model.system_prompt, "first\n  second");
    }

    #[test]
    fn global_side_chat_config_round_trips_with_independent_normalized_values() {
        let source = Utf8Path::new("side-chat.toml");
        let config = ConfigLoader::resolve_global_config_text_without_environment(
            source,
            r#"
[model]
model = "main-model"

[side_chat]
base_url = " https://side.example.test/v1/ "
model = "  side-model  "
system_prompt = "  first\n  second  "
provider_profile = "openai_compatible"
context_window = 65536
request_timeout_ms = 45000
connect_timeout_ms = 5000
max_retries = 4
"#,
        )
        .expect("valid global Side Chat config");

        assert_eq!(config.model.model, "main-model");
        assert_eq!(config.side_chat.base_url, "https://side.example.test/v1");
        assert_eq!(config.side_chat.model, "side-model");
        assert_eq!(config.side_chat.system_prompt, "first\n  second");
        assert_eq!(
            config.side_chat.provider_profile,
            ProviderProfile::OpenAiCompatible
        );
        assert_eq!(config.side_chat.context_window, 65_536);
        assert_eq!(config.side_chat.request_timeout_ms, 45_000);
        assert_eq!(config.side_chat.connect_timeout_ms, 5_000);
        assert_eq!(config.side_chat.max_retries, 4);

        let exported = toml::to_string_pretty(&default_config_patch(&config))
            .expect("export normalized global config");
        let imported = ConfigLoader::resolve_global_config_text_without_environment(
            Utf8Path::new("side-chat-round-trip.toml"),
            &exported,
        )
        .expect("re-import normalized global config");
        assert_eq!(imported.side_chat, config.side_chat);
        assert_eq!(imported.model.model, "main-model");
    }

    #[test]
    fn forward_compatible_adoption_ignores_only_unknown_top_level_sections() {
        let source = Utf8Path::new("externally-updated-config.toml");
        let text = "[model]\nmodel = \"external-model\"\n\n[future]\nflag = \"preserve\"\n";

        let strict = ConfigLoader::resolve_global_config_text_with_environment(source, text)
            .expect_err("strict current-schema adoption rejects unknown sections");
        assert!(strict.to_string().contains("future"));

        let adopted = ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(
            source, text,
        )
        .expect("a running older surface may adopt current sections and preserve newer ones");
        assert_eq!(adopted.resolved_config.model.model, "external-model");
        assert_eq!(
            adopted.preserved_unknown_top_level_sections,
            vec!["future".to_string()]
        );

        let unknown_current_field = "[model]\nmodel = \"external-model\"\nfuture_knob = true\n\n[future]\nflag = \"preserve\"\n";
        let error = ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(
            source,
            unknown_current_field,
        )
        .expect_err("unknown fields inside a current section remain invalid");
        assert!(error.to_string().contains("future_knob"));
    }

    #[test]
    fn legacy_stream_timeout_loads_as_the_canonical_request_timeout() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (name, body) in [
            ("legacy-only", "[model]\nstream_idle_timeout_ms = 3600000\n"),
            (
                "matching-fields",
                "[model]\nrequest_timeout_ms = 3600000\nstream_idle_timeout_ms = 3600000\n",
            ),
        ] {
            let path = Utf8PathBuf::from_path_buf(temp.path().join(format!("{name}.toml")))
                .expect("utf8 path");
            fs::write(&path, body).expect("legacy timeout config");

            let config = ConfigLoader::load_with_global_path(path, None)
                .expect("unambiguous legacy timeout remains compatible");

            assert_eq!(config.model.request_timeout_ms, 3_600_000);
        }
    }

    #[test]
    fn mismatched_legacy_stream_timeout_reports_the_exact_config_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("mismatch.toml")).expect("utf8 path");
        fs::write(
            &path,
            "[model]\nrequest_timeout_ms = 3600000\nstream_idle_timeout_ms = 1800000\n",
        )
        .expect("mismatched legacy timeout config");

        let error = ConfigLoader::load_with_global_path(path.clone(), None)
            .expect_err("mismatched timeout fields must not choose a precedence");
        let diagnostic = error.to_string();

        assert!(diagnostic.contains(path.as_str()));
        assert!(diagnostic.contains("model.request_timeout_ms"));
        assert!(diagnostic.contains("model.stream_idle_timeout_ms"));
        assert!(diagnostic.contains("3600000"));
        assert!(diagnostic.contains("1800000"));
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
            "[model]\nmodel = \"  canonical-model  \"\nrequest_timeout_ms = 3600000\n",
        )
        .expect("canonical model config");
        let canonical = ConfigLoader::load_with_global_path(canonical_path, None)
            .expect("canonical provider runtime config");
        assert_eq!(canonical.model.model, "canonical-model");
        assert_eq!(canonical.model.request_timeout_ms, 3_600_000);

        for (name, body, field) in [
            ("blank-model", "[model]\nmodel = \" \\t \"\n", "model.model"),
            (
                "zero-response-start-timeout",
                "[model]\nrequest_timeout_ms = 0\n",
                "model.request_timeout_ms",
            ),
            (
                "request-timeout-above-public-limit",
                "[model]\nrequest_timeout_ms = 3600001\n",
                "model.request_timeout_ms",
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
    fn legacy_generation_toml_fields_are_typed_but_runtime_inert() {
        let text = r#"
[model]
context_window = 65536
chat_completions_reasoning_parameters = "effort_and_summary"
reasoning_effort = "high"
reasoning_summary = "detailed"
max_output_tokens = 1
temperature = nan
top_p = inf
top_k = 0
presence_penalty = -inf
frequency_penalty = nan
seed = 42
stop_sequences = ["legacy-stop"]
supports_reasoning = true
extra_body_json = { chat_template_kwargs = { enable_thinking = false } }
"#;
        let parsed = parse_global_config_text(Utf8Path::new("legacy-generation.toml"), text)
            .expect("well-typed legacy fields remain readable");
        let encoded = toml::to_string(&parsed).expect("compatibility patch serializes");
        for legacy_key in [
            "chat_completions_reasoning_parameters",
            "reasoning_effort",
            "reasoning_summary",
            "max_output_tokens",
            "temperature",
            "top_p",
            "top_k",
            "presence_penalty",
            "frequency_penalty",
            "seed",
            "stop_sequences",
            "supports_reasoning",
            "extra_body_json",
        ] {
            assert!(
                !encoded.contains(legacy_key),
                "compatibility input must not be generated again: {legacy_key}"
            );
        }

        let resolved = ConfigLoader::resolve_global_config_text_without_environment(
            Utf8Path::new("legacy-generation.toml"),
            text,
        )
        .expect("well-typed legacy generation config is accepted");
        let defaults = ResolvedConfig::default();
        assert_eq!(resolved.model.context_window, 65_536);
        assert_eq!(
            resolved.model.chat_completions_reasoning_parameters,
            defaults.model.chat_completions_reasoning_parameters
        );
        assert_eq!(
            resolved.model.reasoning_effort,
            defaults.model.reasoning_effort
        );
        assert_eq!(
            resolved.model.reasoning_summary,
            defaults.model.reasoning_summary
        );
        assert_eq!(
            resolved.model.max_output_tokens,
            defaults.model.max_output_tokens
        );
        assert_eq!(resolved.model.temperature, defaults.model.temperature);
        assert_eq!(resolved.model.top_p, defaults.model.top_p);
        assert_eq!(resolved.model.top_k, defaults.model.top_k);
        assert_eq!(
            resolved.model.presence_penalty,
            defaults.model.presence_penalty
        );
        assert_eq!(
            resolved.model.frequency_penalty,
            defaults.model.frequency_penalty
        );
        assert_eq!(resolved.model.seed, defaults.model.seed);
        assert_eq!(resolved.model.stop_sequences, defaults.model.stop_sequences);
        assert_eq!(
            resolved.model.supports_reasoning,
            defaults.model.supports_reasoning
        );
        assert_eq!(
            resolved.model.extra_body_json,
            defaults.model.extra_body_json
        );

        for invalid in [
            "[model]\ntemperature = \"0.5\"\n",
            "[model]\nmax_output_tokens = \"4096\"\n",
            "[model]\nstop_sequences = [1]\n",
            "[model]\nunknown_generation_knob = true\n",
        ] {
            ConfigLoader::resolve_global_config_text_without_environment(
                Utf8Path::new("invalid-legacy-generation.toml"),
                invalid,
            )
            .expect_err("wrong types and unknown keys must remain rejected");
        }
    }

    #[cfg(feature = "tauri-desktop")]
    #[test]
    fn imported_config_validation_cannot_be_masked_by_runtime_overrides() {
        let source = Utf8Path::new("config(1).toml");
        let text = "[model]\nrequest_timeout_ms = 0\n";
        let global = parse_global_config_text(source, text).expect("parse import candidate");
        let mut environment = PartialResolvedConfig::default();
        environment.model.get_or_insert_default().request_timeout_ms = Some(1_000);

        ConfigLoader::resolve_config(source, Some(global), Some(environment), None)
            .expect("a later override demonstrates how an invalid file value can be masked");
        let error = ConfigLoader::validate_global_config_text(source, text)
            .expect_err("the selected file must remain invalid on its own");

        assert!(error.to_string().contains("model.request_timeout_ms"));
        assert!(error.to_string().contains(source.as_str()));
    }

    #[test]
    fn legacy_generation_environment_values_are_ignored_without_validation() {
        const CHILD_MARKER: &str = "MOYAI_LEGACY_GENERATION_ENV_TEST_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let resolved = ConfigLoader::resolve_global_config_text_with_environment(
                Utf8Path::new("legacy-generation-env.toml"),
                "[model]\ncontext_window = 65536\n",
            )
            .expect("legacy generation environment values are not parsed");
            let defaults = ResolvedConfig::default();
            assert_eq!(resolved.model.context_window, 65_536);
            assert_eq!(
                resolved.model.max_output_tokens,
                defaults.model.max_output_tokens
            );
            assert_eq!(resolved.model.temperature, defaults.model.temperature);
            assert_eq!(resolved.model.top_p, defaults.model.top_p);
            assert_eq!(resolved.model.top_k, defaults.model.top_k);
            assert_eq!(
                resolved.model.presence_penalty,
                defaults.model.presence_penalty
            );
            assert_eq!(
                resolved.model.frequency_penalty,
                defaults.model.frequency_penalty
            );
            assert_eq!(resolved.model.seed, defaults.model.seed);
            assert_eq!(resolved.model.stop_sequences, defaults.model.stop_sequences);
            assert_eq!(
                resolved.model.supports_reasoning,
                defaults.model.supports_reasoning
            );
            assert_eq!(
                resolved.model.extra_body_json,
                defaults.model.extra_body_json
            );
            return;
        }

        let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
        command
            .args([
                "--exact",
                "config::loader::tests::legacy_generation_environment_values_are_ignored_without_validation",
                "--nocapture",
            ])
            .env(CHILD_MARKER, "1");
        for (name, value) in [
            ("MOYAI_CHAT_COMPLETIONS_REASONING_PARAMETERS", "not-a-mode"),
            ("MOYAI_REASONING_EFFORT", ""),
            ("MOYAI_REASONING_SUMMARY", "not-a-summary"),
            ("MOYAI_MAX_OUTPUT_TOKENS", "not-an-integer"),
            ("MOYAI_TEMPERATURE", "not-a-number"),
            ("MOYAI_TOP_P", "not-a-number"),
            ("MOYAI_TOP_K", "not-an-integer"),
            ("MOYAI_PRESENCE_PENALTY", "not-a-number"),
            ("MOYAI_FREQUENCY_PENALTY", "not-a-number"),
            ("MOYAI_SEED", "not-an-integer"),
            ("MOYAI_STOP_SEQUENCES", "legacy,values"),
            ("MOYAI_SUPPORTS_REASONING", "not-a-boolean"),
            ("MOYAI_EXTRA_BODY_JSON", "not-json"),
        ] {
            command.env(name, value);
        }
        let output = command.output().expect("isolated environment test");
        assert!(
            output.status.success(),
            "isolated environment test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn legacy_request_timeout_environment_override_is_unambiguous() {
        for (canonical, legacy, expected) in [
            (None, Some(3_600_000), 3_600_000),
            (Some(3_600_000), Some(3_600_000), 3_600_000),
            (Some(3_600_000), None, 3_600_000),
        ] {
            let mut patch = PartialResolvedConfig::default();

            apply_request_timeout_env_overrides(&mut patch, canonical, legacy)
                .expect("compatible timeout environment overrides");

            let model = patch.model.expect("model environment patch");
            assert_eq!(model.request_timeout_ms, Some(expected));
            assert_eq!(model.legacy_stream_idle_timeout_ms, None);
        }
    }

    #[test]
    fn mismatched_request_timeout_environment_overrides_are_rejected() {
        let mut patch = PartialResolvedConfig::default();

        let error =
            apply_request_timeout_env_overrides(&mut patch, Some(3_600_000), Some(1_800_000))
                .expect_err("mismatched environment aliases must fail");
        let diagnostic = error.to_string();

        assert!(diagnostic.contains("MOYAI_REQUEST_TIMEOUT_MS"));
        assert!(diagnostic.contains("MOYAI_STREAM_IDLE_TIMEOUT_MS"));
        assert!(diagnostic.contains("3600000"));
        assert!(diagnostic.contains("1800000"));
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
    fn enabled_docling_endpoint_is_validated_without_a_readiness_probe_at_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        let valid_path =
            Utf8PathBuf::from_path_buf(temp.path().join("docling-valid.toml")).expect("utf8");
        fs::write(
            &valid_path,
            "[docling]\nenabled = true\nbase_url = \" https://docling.example.test/api/ \"\n",
        )
        .expect("valid Docling config");

        let valid = ConfigLoader::load_with_global_path(valid_path, None)
            .expect("loading config validates syntax without contacting Docling");
        assert_eq!(valid.docling.base_url, "https://docling.example.test/api");

        for (name, endpoint) in [
            ("relative", "docling.internal"),
            ("scheme", "file:///tmp/docling.sock"),
            ("userinfo", "https://user:super-secret@docling.example.test"),
            ("query", "https://docling.example.test?api_key=hidden"),
        ] {
            let path = Utf8PathBuf::from_path_buf(temp.path().join(format!("docling-{name}.toml")))
                .expect("utf8");
            fs::write(
                &path,
                format!("[docling]\nenabled = true\nbase_url = \"{endpoint}\"\n"),
            )
            .expect("invalid Docling config");

            let error = ConfigLoader::load_with_global_path(path.clone(), None)
                .expect_err("invalid enabled Docling endpoint must fail closed");
            let diagnostic = format!("{error:?}: {error}");
            assert!(diagnostic.contains("docling.base_url"));
            assert!(diagnostic.contains(path.as_str()));
            assert!(!diagnostic.contains("super-secret"));
            assert!(!diagnostic.contains("hidden"));
            assert!(!diagnostic.contains(endpoint));
        }

        let disabled_path =
            Utf8PathBuf::from_path_buf(temp.path().join("docling-disabled.toml")).expect("utf8");
        fs::write(
            &disabled_path,
            "[docling]\nenabled = false\nbase_url = \"inactive-draft\"\n",
        )
        .expect("disabled Docling config");
        let disabled = ConfigLoader::load_with_global_path(disabled_path, None)
            .expect("disabled Docling keeps its inactive draft value");
        assert_eq!(disabled.docling.base_url, "inactive-draft");
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
    fn legacy_provider_mode_pairs_normalize_to_all_canonical_profiles() {
        for (metadata, api, expected) in [
            (
                "lm_studio_native_required",
                "responses",
                ProviderProfile::LmStudio,
            ),
            (
                "lm_studio_native_required",
                "chat_completions",
                ProviderProfile::LmStudioChatCompletions,
            ),
            (
                "openai_compatible_only",
                "responses",
                ProviderProfile::OpenAiResponses,
            ),
            (
                "openai_compatible_only",
                "chat_completions",
                ProviderProfile::OpenAiCompatible,
            ),
        ] {
            let text = format!(
                "[model]\nprovider_metadata_mode = \"{metadata}\"\nprovider_api_mode = \"{api}\"\n"
            );
            let resolved = ConfigLoader::resolve_global_config_text_without_environment(
                Utf8Path::new("legacy-provider.toml"),
                &text,
            )
            .expect("legacy provider pair");
            assert_eq!(resolved.model.provider_profile, expected);
        }
    }

    #[test]
    fn sparse_legacy_provider_override_preserves_the_inherited_profile_axis() {
        let global = toml::from_str::<PartialResolvedConfig>(
            "[model]\nprovider_profile = \"openai_compatible\"\n",
        )
        .expect("canonical global profile");
        let environment =
            toml::from_str::<PartialResolvedConfig>("[model]\nprovider_api_mode = \"responses\"\n")
                .expect("legacy environment override");

        let resolved = ConfigLoader::resolve_config(
            Utf8Path::new("layered-provider.toml"),
            Some(global),
            Some(environment),
            None,
        )
        .expect("layered compatibility resolution");

        assert_eq!(
            resolved.model.provider_profile,
            ProviderProfile::OpenAiResponses
        );
    }

    #[test]
    fn cli_provider_connection_is_one_precedence_layer_over_environment() {
        let environment = PartialResolvedConfig {
            model: Some(PartialModelConfig {
                base_url: Some("https://provider-a.example/v1".to_string()),
                provider_profile: Some(ProviderProfile::LmStudio),
                api_key_env: Some(Some("PROVIDER_A_KEY".to_string())),
                extra_headers: Some(std::collections::BTreeMap::from([(
                    "Authorization".to_string(),
                    "Bearer provider-a-secret".to_string(),
                )])),
                extra_body_json: Some(serde_json::json!({"provider": "a"})),
                ..PartialModelConfig::default()
            }),
            ..PartialResolvedConfig::default()
        };
        let args = RunArgs {
            prompt: None,
            session_id: None,
            continue_last: false,
            title: None,
            directory: None,
            model_override: None,
            provider_connection_override: crate::cli::ProviderConnectionOverrideArgs {
                base_url: Some("https://provider-b.example/v1".to_string()),
                provider_profile: Some(ProviderProfile::OpenAiCompatible),
                api_key_env: Some("PROVIDER_B_KEY".to_string()),
            },
            output_mode: crate::cli::OutputMode::Json,
            show_reasoning_summary: false,
            review_uncommitted: false,
            review_branch: None,
            active_file: None,
            open_tabs: Vec::new(),
            visible_files: Vec::new(),
            image_paths: Vec::new(),
        };

        let explicit = ConfigLoader::resolve_config(
            Utf8Path::new("cli-provider.toml"),
            None,
            Some(environment.clone()),
            Some(&args),
        )
        .expect("atomic CLI provider connection");

        assert_eq!(explicit.model.base_url, "https://provider-b.example/v1");
        assert_eq!(
            explicit.model.provider_profile,
            ProviderProfile::OpenAiCompatible
        );
        assert_eq!(
            explicit.model.api_key_env.as_deref(),
            Some("PROVIDER_B_KEY")
        );
        assert!(explicit.model.extra_headers.is_empty());
        assert_eq!(explicit.model.extra_body_json, None);

        let url_and_key_args = RunArgs {
            provider_connection_override: crate::cli::ProviderConnectionOverrideArgs {
                base_url: Some("https://provider-keyed.example/v1".to_string()),
                api_key_env: Some("PROVIDER_B_KEY".to_string()),
                ..Default::default()
            },
            ..args.clone()
        };
        let url_and_key = ConfigLoader::resolve_config(
            Utf8Path::new("cli-provider.toml"),
            None,
            Some(environment.clone()),
            Some(&url_and_key_args),
        )
        .expect("URL and API-key name belong to the same CLI patch");

        assert_eq!(
            url_and_key.model.base_url,
            "https://provider-keyed.example/v1"
        );
        assert_eq!(
            url_and_key.model.provider_profile,
            ProviderProfile::LmStudio
        );
        assert_eq!(
            url_and_key.model.api_key_env.as_deref(),
            Some("PROVIDER_B_KEY")
        );
        assert!(url_and_key.model.extra_headers.is_empty());
        assert_eq!(url_and_key.model.extra_body_json, None);

        let base_only_args = RunArgs {
            provider_connection_override: crate::cli::ProviderConnectionOverrideArgs {
                base_url: Some("https://provider-c.example/v1".to_string()),
                ..Default::default()
            },
            ..args
        };
        let base_only = ConfigLoader::resolve_config(
            Utf8Path::new("cli-provider.toml"),
            None,
            Some(environment),
            Some(&base_only_args),
        )
        .expect("base-only CLI override");

        assert_eq!(base_only.model.base_url, "https://provider-c.example/v1");
        assert_eq!(base_only.model.api_key_env, None);
        assert!(base_only.model.extra_headers.is_empty());
        assert_eq!(base_only.model.extra_body_json, None);
    }

    #[test]
    fn canonical_provider_profile_rejects_a_conflicting_legacy_field() {
        let error = ConfigLoader::resolve_global_config_text_without_environment(
            Utf8Path::new("conflicting-provider.toml"),
            "[model]\nprovider_profile = \"openai_compatible\"\nprovider_api_mode = \"responses\"\n",
        )
        .expect_err("conflicting canonical and legacy profile must fail");

        let message = error.to_string();
        assert!(message.contains("model.provider_profile"), "{message}");
        assert!(message.contains("model.provider_api_mode"), "{message}");
    }

    #[test]
    fn legacy_provider_api_mode_environment_input_still_selects_the_connection_format() {
        let mut patch = PartialResolvedConfig::default();
        patch.model.get_or_insert_default().provider_api_mode =
            parse_provider_api_mode("chat-completions");

        let resolved = apply_patch(ResolvedConfig::default(), patch);
        assert_eq!(
            resolved.model.provider_profile,
            ProviderProfile::LmStudioChatCompletions
        );
    }

    #[test]
    fn provider_api_mode_environment_parser_covers_legacy_connection_values() {
        assert_eq!(
            parse_provider_api_mode("auto"),
            Some(ProviderApiMode::Responses)
        );
        assert_eq!(
            parse_provider_api_mode("responses"),
            Some(ProviderApiMode::Responses)
        );
        assert_eq!(parse_provider_api_mode("invalid"), None);
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

fn parse_access_mode(value: &str) -> Option<AccessMode> {
    AccessMode::parse(value)
}

fn parse_string_map_json(value: &str) -> Option<std::collections::BTreeMap<String, String>> {
    serde_json::from_str(value).ok()
}
