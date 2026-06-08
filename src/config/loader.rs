use std::env;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;

use crate::cli::RunArgs;
use crate::config::merge::apply_patch;
use crate::config::model::{
    AccessMode, PartialAgentConfig, PartialDoclingConfig, PartialFileGuardConfig,
    PartialFormatConfig, PartialInspectionConfig, PartialInstructionConfig, PartialLoggingConfig,
    PartialMcpConfig, PartialModelConfig, PartialPermissionsConfig, PartialResolvedConfig,
    PartialSessionConfig, PartialShellConfig, PartialToolOutputConfig, PartialWorkspaceConfig,
    ResolvedConfig,
};
use crate::error::ConfigError;

const GLOBAL_CONFIG_PATH_ENV: &str = "MOYAI_CONFIG_PATH";

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
        let mut resolved = ResolvedConfig::default();

        if let Some(global) = read_optional(global_config_path)? {
            resolved = apply_patch(resolved, global);
        }

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
    let text = fs::read_to_string(path)?;
    let parsed = toml::from_str::<PartialResolvedConfig>(&text)?;
    Ok(Some(parsed))
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
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, encoded)?;
    if path.exists() {
        let _ = fs::remove_file(&temp_path);
        return Ok(());
    }
    fs::rename(&temp_path, path)?;
    Ok(())
}

fn default_config_patch(config: &ResolvedConfig) -> PartialResolvedConfig {
    PartialResolvedConfig {
        model: Some(PartialModelConfig {
            base_url: Some(config.model.base_url.clone()),
            model: Some(config.model.model.clone()),
            prompt_profile: Some(config.model.prompt_profile),
            provider_metadata_mode: Some(config.model.provider_metadata_mode),
            api_key_env: Some(config.model.api_key_env.clone()),
            extra_headers: Some(config.model.extra_headers.clone()),
            request_timeout_ms: Some(config.model.request_timeout_ms),
            stream_idle_timeout_ms: Some(config.model.stream_idle_timeout_ms),
            connect_timeout_ms: Some(config.model.connect_timeout_ms),
            max_retries: Some(config.model.max_retries),
            stream_max_retries: Some(config.model.stream_max_retries),
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
            default_title_max_len: Some(config.session.default_title_max_len),
            transcript_limit_messages: Some(config.session.transcript_limit_messages),
            auto_resume_last: Some(config.session.auto_resume_last),
            max_steps_per_turn: Some(config.session.max_steps_per_turn),
            overflow_margin_tokens: Some(config.session.overflow_margin_tokens),
        }),
        agent: Some(PartialAgentConfig {
            duplicate_success_abort_threshold: Some(config.agent.duplicate_success_abort_threshold),
            repetitive_text_line_threshold: Some(config.agent.repetitive_text_line_threshold),
            readonly_stall_threshold_implementation: Some(
                config.agent.readonly_stall_threshold_implementation,
            ),
            readonly_stall_threshold_general: Some(config.agent.readonly_stall_threshold_general),
            verification_repair_grace_steps: Some(config.agent.verification_repair_grace_steps),
            verification_failure_attempt_limit: Some(
                config.agent.verification_failure_attempt_limit,
            ),
            verification_failure_repair_read_budget: Some(
                config.agent.verification_failure_repair_read_budget,
            ),
            staged_task_documentation_finish_grace_steps: Some(
                config.agent.staged_task_documentation_finish_grace_steps,
            ),
            staged_task_discovery_redirect_repeat_threshold: Some(
                config.agent.staged_task_discovery_redirect_repeat_threshold,
            ),
            staged_task_authoring_read_limit: Some(config.agent.staged_task_authoring_read_limit),
            staged_task_authoring_successful_read_budget_after_progress: Some(
                config
                    .agent
                    .staged_task_authoring_successful_read_budget_after_progress,
            ),
            staged_task_audit_repair_read_budget: Some(
                config.agent.staged_task_audit_repair_read_budget,
            ),
            staged_task_audit_repair_rewrite_escalation_threshold: Some(
                config
                    .agent
                    .staged_task_audit_repair_rewrite_escalation_threshold,
            ),
            staged_task_recovery_stall_threshold: Some(
                config.agent.staged_task_recovery_stall_threshold,
            ),
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
            api_key_env: Some(config.docling.api_key_env.clone()),
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
    if let Ok(value) = env::var("MOYAI_PROMPT_PROFILE") {
        if let Some(parsed) = parse_prompt_profile(&value) {
            patch.model.get_or_insert_default().prompt_profile = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_PROVIDER_METADATA_MODE") {
        if let Some(parsed) = parse_provider_metadata_mode(&value) {
            patch.model.get_or_insert_default().provider_metadata_mode = Some(parsed);
        }
    }
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
    if let Ok(value) = env::var("MOYAI_STREAM_MAX_RETRIES") {
        if let Ok(parsed) = value.parse() {
            patch.model.get_or_insert_default().stream_max_retries = Some(parsed);
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
    if let Ok(value) = env::var("MOYAI_TRANSCRIPT_LIMIT_MESSAGES") {
        if let Ok(parsed) = value.parse() {
            patch
                .session
                .get_or_insert_default()
                .transcript_limit_messages = Some(parsed);
        }
    }
    if let Ok(value) = env::var("MOYAI_MAX_STEPS_PER_TURN") {
        if let Ok(parsed) = value.parse() {
            patch.session.get_or_insert_default().max_steps_per_turn = Some(parsed);
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
        assert!(text.contains("max_output_tokens = 8192"));
        assert!(text.contains("[docling]"));
        assert!(text.contains("enabled = false"));
        assert!(text.contains("base_url = \"http://127.0.0.1:8123\""));
        assert!(text.contains("base_url = \"http://127.0.0.1:8123/mcp\""));
        assert!(text.contains("[permissions]"));
        toml::from_str::<PartialResolvedConfig>(&text).expect("generated config parses");
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
}

fn parse_prompt_profile(value: &str) -> Option<crate::config::model::PromptProfile> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(crate::config::model::PromptProfile::Auto),
        "default" => Some(crate::config::model::PromptProfile::Default),
        "qwen_coder" | "qwen-coder" | "qwen" => {
            Some(crate::config::model::PromptProfile::QwenCoder)
        }
        _ => None,
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

fn parse_access_mode(value: &str) -> Option<AccessMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "default" | "normal" => Some(AccessMode::Default),
        "auto_review" | "auto-review" | "autoreview" | "auto" => Some(AccessMode::AutoReview),
        "full_access" | "full-access" | "full" => Some(AccessMode::FullAccess),
        _ => None,
    }
}

fn parse_string_map_json(value: &str) -> Option<std::collections::BTreeMap<String, String>> {
    serde_json::from_str(value).ok()
}
