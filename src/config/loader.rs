use std::env;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;

use crate::cli::RunArgs;
use crate::config::merge::apply_patch;
use crate::config::model::{AccessMode, PartialResolvedConfig, ResolvedConfig};
use crate::error::ConfigError;
use crate::workspace::project::find_workspace_root;

pub struct ConfigLoader;

impl ConfigLoader {
    pub fn load(
        start_dir: &Utf8Path,
        cli: Option<&RunArgs>,
    ) -> Result<ResolvedConfig, ConfigError> {
        let mut resolved = ResolvedConfig::default();

        if let Some(global) = read_optional(global_config_path()?)? {
            resolved = apply_patch(resolved, global);
        }

        let project_root = find_workspace_root(start_dir)
            .map_err(|error| ConfigError::Workspace(error.to_string()))?
            .unwrap_or_else(|| start_dir.to_path_buf());
        for candidate in project_config_paths(&project_root) {
            if let Some(project) = read_optional(candidate)? {
                resolved = apply_patch(resolved, project);
            }
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
}

pub fn global_config_path() -> Result<Utf8PathBuf, ConfigError> {
    let dirs = ProjectDirs::from("net", "midi-ai-labs", "moyai")
        .ok_or_else(|| ConfigError::Message("failed to resolve config directory".to_string()))?;
    let config_dir = Utf8PathBuf::from_path_buf(dirs.config_dir().to_path_buf())
        .map_err(|_| ConfigError::Message("config directory is not valid UTF-8".to_string()))?;
    Ok(config_dir.join("config.toml"))
}

pub fn project_config_paths(root: &Utf8Path) -> [Utf8PathBuf; 2] {
    [
        root.join("moyai.toml"),
        root.join(".moyai").join("config.toml"),
    ]
}

fn read_optional(path: Utf8PathBuf) -> Result<Option<PartialResolvedConfig>, ConfigError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    let parsed = toml::from_str::<PartialResolvedConfig>(&text)?;
    Ok(Some(parsed))
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
